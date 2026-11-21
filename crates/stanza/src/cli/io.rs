use std::ffi::OsString;
use std::fmt::Display;
use std::io::{self, IsTerminal, LineWriter, Read, Write};
use std::os::fd::RawFd;

use super::hook::HookExit;
use super::look::{Color, Look, Style};
use super::report::{Report, write_findings};
use crate::format::Findings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Stanza,
    Hook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Error,
    Notice,
}

impl Program {
    pub(crate) fn line(self, tone: Tone, color: Color, message: impl Display) -> String {
        let name = match self {
            Program::Stanza => "stanza:",
            Program::Hook => "stanza hook:",
        };

        let style = match tone {
            Tone::Error => Style::BoldRed,
            Tone::Notice => Style::BoldYellow,
        };

        format!("{} {message}", color.paint(style, name))
    }

    pub fn failure_code(self) -> i32 {
        match self {
            Program::Stanza => 2,
            Program::Hook => HookExit::Failed.code(),
        }
    }
}

pub struct Io {
    pub cwd: Result<String, String>,
    pub stack: Option<usize>,
    pub workers: Option<String>,
    pub agent_hooks: Option<String>,
    pub look: Look,
    pub stdin: Box<dyn FnMut() -> io::Result<Vec<u8>>>,
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    pub unwritten: Option<Unwritten>,
}

pub struct Unwritten {
    pub(crate) error: io::Error,
    pub(crate) lost: Lost,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Lost {
    Notice,
    Results,
}

impl Io {
    pub fn system(
        cwd: Result<String, String>,
        stack: Option<usize>,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Self {
        Io {
            cwd,
            stack,
            workers: env("STANZA_WORKERS").map(|value| value.to_string_lossy().into_owned()),
            agent_hooks: env("AGENT_HOOKS").map(|value| value.to_string_lossy().into_owned()),
            look: Look::of(&env, io::stdout().is_terminal(), io::stderr().is_terminal()),
            stdin: Box::new(|| {
                let mut bytes = Vec::new();
                io::stdin().read_to_end(&mut bytes).map(|_| bytes)
            }),
            stdout: Box::new(LineWriter::new(Waiting::on(libc::STDOUT_FILENO))),
            stderr: Box::new(Waiting::on(libc::STDERR_FILENO)),
            unwritten: None,
        }
    }

    pub(crate) fn record(&mut self, result: io::Result<()>, lost: Lost) {
        let Err(error) = result else {
            return;
        };

        if error.kind() == io::ErrorKind::BrokenPipe {
            return;
        }

        match &mut self.unwritten {
            Some(unwritten) => unwritten.lost = unwritten.lost.max(lost),
            None => self.unwritten = Some(Unwritten { error, lost }),
        }
    }

    pub(crate) fn out(&mut self, bytes: &[u8]) {
        let result = self.stdout.write_all(bytes);
        self.record(result, Lost::Results);
    }

    fn err(&mut self, bytes: &[u8], lost: Lost) {
        let result = self.stderr.write_all(bytes);
        self.record(result, lost);
    }

    pub(crate) fn warn(&mut self, line: &str) {
        self.err(format!("{line}\n").as_bytes(), Lost::Notice);
    }

    pub(crate) fn error(&mut self, program: Program, message: impl Display) {
        self.warn(&program.line(Tone::Error, self.look.stderr, message));
    }

    pub(crate) fn notice(&mut self, program: Program, message: impl Display) {
        self.warn(&program.line(Tone::Notice, self.look.stderr, message));
    }

    pub(crate) fn summary(&mut self, line: &str) {
        self.warn(&self.look.stderr.numbers(line));
    }

    pub(crate) fn out_findings(&mut self, findings: &Findings, report: Report) {
        let result = write_findings(&mut self.stdout, findings, report);
        self.record(result, Lost::Results);
    }
}

pub(crate) struct Waiting<W, F> {
    inner: W,
    ready: F,
}

impl Waiting<Descriptor, Box<dyn FnMut() -> io::Result<()>>> {
    fn on(fd: RawFd) -> Self {
        Waiting {
            inner: Descriptor(fd),
            ready: Box::new(move || writable(fd)),
        }
    }
}

impl<W, F: FnMut() -> io::Result<()>> Waiting<W, F> {
    fn retry<T>(&mut self, mut attempt: impl FnMut(&mut W) -> io::Result<T>) -> io::Result<T> {
        loop {
            match attempt(&mut self.inner) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.wait()?,
                done => return done,
            }
        }
    }

    fn wait(&mut self) -> io::Result<()> {
        loop {
            match (self.ready)() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                waited => return waited,
            }
        }
    }
}

impl<W: Write, F: FnMut() -> io::Result<()>> Write for Waiting<W, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.retry(|inner| inner.write(bytes))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.retry(W::flush)
    }
}

fn writable(fd: RawFd) -> io::Result<()> {
    let mut wanted = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };

    match unsafe { libc::poll(&mut wanted, 1, -1) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

pub(crate) struct Descriptor(RawFd);

impl Write for Descriptor {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = bytes.len().min(libc::c_int::MAX as usize - 1);
        let written = unsafe { libc::write(self.0, bytes.as_ptr().cast(), length) };
        if written >= 0 {
            return Ok(written as usize);
        }

        let error = io::Error::last_os_error();
        // std's stdout and stderr treat a closed descriptor as a sink, so `stanza >&-` still succeeds.
        match error.raw_os_error() {
            Some(libc::EBADF) => Ok(bytes.len()),
            _ => Err(error),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;
    use crate::cli::run;
    use crate::pool::STACK_LADDER;

    enum Reply {
        Accept(usize),
        Block,
        Fail(io::Error),
    }

    #[derive(Default)]
    struct Pipe {
        replies: VecDeque<Reply>,
        received: Vec<u8>,
        waits: usize,
    }

    #[derive(Clone)]
    struct Scripted(Rc<RefCell<Pipe>>);

    impl Scripted {
        fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
            Scripted(Rc::new(RefCell::new(Pipe {
                replies: replies.into_iter().collect(),
                ..Pipe::default()
            })))
        }

        fn waiting(&self) -> Waiting<Scripted, impl FnMut() -> io::Result<()> + use<>> {
            let pipe = self.0.clone();
            Waiting {
                inner: self.clone(),
                ready: move || {
                    pipe.borrow_mut().waits += 1;
                    Ok(())
                },
            }
        }
    }

    impl Write for Scripted {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut pipe = self.0.borrow_mut();
            let taken = match pipe.replies.pop_front() {
                Some(Reply::Accept(limit)) => limit.min(bytes.len()),
                Some(Reply::Block) => return Err(io::ErrorKind::WouldBlock.into()),
                Some(Reply::Fail(error)) => return Err(error),
                None => bytes.len(),
            };

            pipe.received.extend_from_slice(&bytes[..taken]);
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    const BRACED: &str = "if (a) {\n  b();\n}\nc();\n";

    fn check_into(stdout: Box<dyn Write>) -> (i32, String) {
        let stderr = Scripted::new([]);
        let mut io = Io {
            cwd: Ok("/".to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
            look: Look::default(),
            stdin: Box::new(|| Ok(BRACED.as_bytes().to_vec())),
            stdout,
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args = ["--check", "--stdin", "x.ts"].map(str::to_owned);
        let code = run(&args, &mut io);
        let stderr = String::from_utf8(stderr.0.take().received).unwrap();
        (code, stderr)
    }

    #[test]
    fn output_retry_preserves_progress_through_a_short_write_and_a_full_pipe() {
        let pipe = Scripted::new([Reply::Accept(3), Reply::Block, Reply::Accept(4)]);
        let mut out = LineWriter::new(pipe.waiting());
        out.write_all(b"first line\nsecond line\n").unwrap();
        out.flush().unwrap();

        let pipe = pipe.0.take();
        assert_eq!(pipe.received, b"first line\nsecond line\n");
        assert_eq!(pipe.waits, 1);
    }

    #[test]
    fn a_report_stdout_cannot_take_ends_with_the_write_failure_and_no_summary() {
        let dir = std::env::temp_dir().join(format!("stanza-unwritten-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.ts");
        std::fs::write(&file, BRACED).unwrap();

        let stdout = Scripted::new([Reply::Fail(io::Error::from_raw_os_error(libc::ENOSPC))]);
        let stderr = Scripted::new([]);
        let mut io = Io {
            cwd: Ok("/".to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
            look: Look::default(),
            stdin: Box::new(|| Ok(Vec::new())),
            stdout: Box::new(LineWriter::new(stdout)),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args = ["--check".to_owned(), file.to_string_lossy().into_owned()];
        let code = run(&args, &mut io);
        let stderr = String::from_utf8(stderr.0.take().received).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(code, 2);
        assert_eq!(
            stderr,
            "stanza: cannot write output: no space left on device\n"
        );
    }

    #[test]
    fn output_retry_preserves_progress_when_a_wait_is_interrupted() {
        let pipe = Scripted::new([Reply::Block]);
        let mut waits = 0;
        let mut out = Waiting {
            inner: pipe.clone(),
            ready: || {
                waits += 1;
                match waits {
                    1 => Err(io::ErrorKind::Interrupted.into()),
                    _ => Ok(()),
                }
            },
        };

        out.write_all(b"report\n").unwrap();
        assert_eq!(waits, 2);
        assert_eq!(pipe.0.take().received, b"report\n");
    }

    #[test]
    fn output_retry_preserves_progress_until_a_closed_pipe_keeps_the_run_code() {
        let closed = io::Error::from(io::ErrorKind::BrokenPipe);
        let pipe = Scripted::new([Reply::Block, Reply::Fail(closed)]);
        let (code, stderr) = check_into(Box::new(LineWriter::new(pipe.waiting())));
        assert_eq!((code, stderr.as_str()), (1, ""));
        assert_eq!(pipe.0.take().waits, 1);
    }

    #[test]
    fn output_retry_preserves_progress_until_a_permanent_error_fails_the_run() {
        let full = io::Error::from_raw_os_error(libc::ENOSPC);
        let pipe = Scripted::new([Reply::Block, Reply::Fail(full)]);
        let (code, stderr) = check_into(Box::new(LineWriter::new(pipe.waiting())));
        assert_eq!(code, 2);
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert!(
            stderr.starts_with("stanza: cannot write output: "),
            "{stderr}"
        );
        assert_eq!(pipe.0.take().waits, 1);
    }
}
