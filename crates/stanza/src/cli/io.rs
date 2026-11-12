use std::ffi::OsString;
use std::fmt::Display;
use std::io::{self, Read, Write};

use super::report::write_findings;
use crate::format::Findings;
use crate::hook::HookExit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Stanza,
    Hook,
}

impl Program {
    pub fn line(self, message: impl Display) -> String {
        let name = match self {
            Program::Stanza => "stanza",
            Program::Hook => "stanza hook",
        };

        format!("{name}: {message}")
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
            stdin: Box::new(|| {
                let mut bytes = Vec::new();
                io::stdin().read_to_end(&mut bytes).map(|_| bytes)
            }),
            stdout: Box::new(io::stdout()),
            stderr: Box::new(io::stderr()),
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
        self.warn(&program.line(message));
    }

    pub(crate) fn out_findings(&mut self, findings: &Findings, json: bool) {
        let result = write_findings(&mut self.stdout, findings, json);
        self.record(result, Lost::Results);
    }
}
