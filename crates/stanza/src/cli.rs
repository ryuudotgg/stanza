use std::collections::HashSet;
use std::io::{self, Write};

use crate::engine::types::{Finding, Mode};
use crate::files::{
    EmptyReason, Locations, StdinTarget, collect_files, empty_reason, error_code, stdin_target,
};
use crate::format::{Formatted, Input, format_inputs, read_text};
use crate::paths::relative;
use crate::step::decode;
use crate::usage::{BOTH_BRACES, USAGE};

pub const VERSION: &str = env!("STANZA_VERSION");

pub const COMMIT: Option<&str> = option_env!("STANZA_COMMIT");

pub struct Io {
    pub cwd: String,
    pub stdin: Box<dyn FnMut() -> io::Result<Vec<u8>>>,
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    pub unwritten: Option<io::Error>,
}

impl Io {
    fn record(&mut self, result: io::Result<()>) {
        match result {
            Err(error) if error.kind() != io::ErrorKind::BrokenPipe => {
                self.unwritten.get_or_insert(error);
            }
            _ => {}
        }
    }

    fn out(&mut self, bytes: &[u8]) {
        let result = self.stdout.write_all(bytes);
        self.record(result);
    }

    fn err(&mut self, bytes: &[u8]) {
        let result = self.stderr.write_all(bytes);
        self.record(result);
    }

    fn warn(&mut self, line: &str) {
        self.err(format!("{line}\n").as_bytes());
    }
}

pub fn version_line(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("stanza {version} ({commit})\n"),
        None => format!("stanza {version}\n"),
    }
}

struct Arguments {
    changed: bool,
    staged: bool,
    json: bool,
    mode: Mode,
    paths: Vec<String>,
    stdin: Option<String>,
}

const SWITCHES: [&str; 6] = [
    "--braces",
    "--changed",
    "--hunks",
    "--json",
    "--no-braces",
    "--staged",
];

const STANDALONE: [&str; 3] = ["--help", "-h", "--version"];

fn parse_arguments(args: &[String]) -> Result<Arguments, String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut mode = None;
    let mut stdin = None;
    let mut queue = args.iter();
    while let Some(arg) = queue.next() {
        let arg = arg.as_str();
        if arg == "--" {
            paths.extend(queue.by_ref().cloned());
            break;
        }

        if arg == "--fix" || arg == "--check" {
            if mode.is_some() {
                return Err("use one of --fix or --check".to_owned());
            }

            mode = Some(if arg == "--fix" {
                Mode::Fix
            } else {
                Mode::Check
            });
            continue;
        }

        if SWITCHES.contains(&arg) {
            if !seen.insert(arg) {
                return Err(format!("{arg} given twice"));
            }

            continue;
        }

        if arg == "--stdin" {
            let path = queue.next();
            if stdin.is_some() {
                return Err("--stdin given twice".to_owned());
            }

            match path {
                Some(path) if !path.starts_with('-') => stdin = Some(path.clone()),
                _ => return Err("--stdin needs a path".to_owned()),
            }

            continue;
        }

        if STANDALONE.contains(&arg) {
            return Err(format!("{arg} takes no other arguments"));
        }

        if arg.starts_with('-') {
            return Err(format!("unknown flag {arg}"));
        }

        paths.push(arg.to_owned());
    }

    let changed = seen.contains("--changed");
    let hunks = seen.contains("--hunks");
    let staged = seen.contains("--staged");
    let sources = [changed, staged, !paths.is_empty(), stdin.is_some()]
        .into_iter()
        .filter(|&given| given)
        .count();

    let Some(mode) = mode else {
        return Err("--fix or --check is required".to_owned());
    };

    if sources == 0 {
        return Err(
            "nothing to format: give paths, --changed, --staged or --stdin <path>".to_owned(),
        );
    }

    if sources > 1 {
        return Err("use only one of --changed, --staged, --stdin or paths".to_owned());
    }

    if staged && mode == Mode::Fix {
        return Err("--staged works only with --check".to_owned());
    }

    if hunks && !changed && !staged {
        return Err("--hunks needs --changed or --staged".to_owned());
    }

    if seen.contains("--braces") && seen.contains("--no-braces") {
        return Err(BOTH_BRACES.to_owned());
    }

    Ok(Arguments {
        changed,
        staged,
        json: seen.contains("--json"),
        mode,
        paths,
        stdin,
    })
}

fn printed_path(path: &str, cwd: &str) -> String {
    let output = relative(cwd, path);
    if output.is_empty() {
        path.to_owned()
    } else {
        output
    }
}

fn printed_findings(findings: &[Finding], json: bool) -> Vec<u8> {
    if json {
        let mut line = serde_json::to_vec(findings).expect("findings serialize");
        line.push(b'\n');
        return line;
    }

    findings
        .iter()
        .flat_map(|finding| {
            format!(
                "{}:{}:{} {} {}\n",
                finding.path,
                finding.line,
                finding.col,
                finding.rule.id(),
                finding.message
            )
            .into_bytes()
        })
        .collect()
}

fn status(errors: &[String], formatted: &Formatted) -> i32 {
    if !errors.is_empty() || formatted.failed {
        return 2;
    }

    i32::from(!formatted.findings.is_empty())
}

fn unsupported(io: &mut Io, feature: &str) -> i32 {
    io.warn(&format!(
        "stanza: this build does not support {feature} yet"
    ));
    2
}

fn run_stdin(input: &str, args: &Arguments, io: &mut Io) -> i32 {
    let source = match (io.stdin)() {
        Ok(source) => source,
        Err(error) => {
            let code = error_code(&error).map_or_else(|| error.to_string(), str::to_owned);
            io.warn(&format!("stanza: cannot read stdin: {code}"));
            return 2;
        }
    };

    let target = stdin_target(input, &io.cwd, &mut Locations::default());
    let inputs = match &target {
        StdinTarget::Format(path) => vec![Input {
            path: path.clone(),
            output: printed_path(path, &io.cwd),
            text: decode(source.clone()),
        }],
        _ => Vec::new(),
    };

    let formatted = format_inputs(inputs.into_iter());
    let printed = printed_findings(&formatted.findings, args.json);
    if args.mode == Mode::Fix {
        io.out(&source);
        io.err(&printed);
    } else {
        io.out(&printed);
    }

    let errors = match target {
        StdinTarget::Unsupported(error) | StdinTarget::Failed(error) => vec![error],
        StdinTarget::Format(_) | StdinTarget::Skip => Vec::new(),
    };

    for error in &errors {
        io.warn(&format!("stanza: {error}"));
    }

    status(&errors, &formatted)
}

fn empty_message(reason: &EmptyReason, inputs: &[String]) -> String {
    let paths = inputs.join(", ");
    match reason {
        EmptyReason::Generated => {
            format!("stanza: nothing to format in {paths}, generated files are skipped")
        }
        EmptyReason::Skipped => {
            format!("stanza: nothing to format in {paths}, its files are always skipped")
        }
        EmptyReason::None => format!("stanza: no TypeScript or JavaScript files under {paths}"),
    }
}

fn run_files(args: &Arguments, io: &mut Io) -> i32 {
    let mut locations = Locations::default();
    let collected = collect_files(&args.paths, &io.cwd, false, &mut locations);
    let explains_generated = collected.errors.is_empty();
    let reason = (collected.files.is_empty() && collected.errors.is_empty())
        .then(|| empty_reason(&args.paths, &io.cwd, &mut locations));

    for warning in &collected.warnings {
        io.warn(&format!("stanza: {warning}"));
    }

    if let Some(reason) = &reason {
        io.warn(&empty_message(reason, &args.paths));
    }

    let cwd = io.cwd.clone();
    let formatted = format_inputs(collected.files.into_iter().map(|path| Input {
        output: printed_path(&path, &cwd),
        text: read_text(&path),
        path,
    }));

    if explains_generated && formatted.only_generated {
        io.warn(&empty_message(&EmptyReason::Generated, &args.paths));
    }

    io.out(&printed_findings(&formatted.findings, args.json));

    for error in &collected.errors {
        io.warn(&format!("stanza: {error}"));
    }

    status(&collected.errors, &formatted)
}

pub fn run(argv: &[String], io: &mut Io) -> i32 {
    let code = dispatch(argv, io);
    let flushed = io.stdout.flush();
    io.record(flushed);

    match &io.unwritten {
        Some(error) => {
            let _ = writeln!(io.stderr, "stanza: cannot write output: {error}");
            1
        }
        None => code,
    }
}

fn dispatch(argv: &[String], io: &mut Io) -> i32 {
    match argv.first().map(String::as_str) {
        Some("hook") => return unsupported(io, "hook"),
        Some("explain") => return unsupported(io, "explain"),
        Some(only @ ("--help" | "-h")) if argv.len() == 1 => return unsupported(io, only),
        Some("--version") if argv.len() == 1 => {
            io.out(version_line(VERSION, COMMIT).as_bytes());

            return 0;
        }
        _ => {}
    }

    let args = match parse_arguments(argv) {
        Ok(args) => args,
        Err(error) => {
            io.warn(&format!("stanza: {error}\n{USAGE}"));
            return 2;
        }
    };

    if let Some(input) = &args.stdin {
        return run_stdin(input, &args, io);
    }

    if args.staged {
        return unsupported(io, "--staged");
    }

    if args.changed {
        return unsupported(io, "--changed");
    }

    run_files(&args, io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Captured(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn run_with(args: &[&str], stdin: &[u8]) -> (Vec<u8>, String, i32) {
        let stdin = stdin.to_vec();
        let (stdout, stderr) = (Captured::default(), Captured::default());
        let mut io = Io {
            cwd: std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            stdin: Box::new(move || Ok(stdin.clone())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
        let code = run(&args, &mut io);
        let stderr = String::from_utf8(stderr.0.take()).unwrap();
        (stdout.0.take(), stderr, code)
    }

    #[test]
    fn version_line_without_commit() {
        assert_eq!(version_line("1.2.3", None), "stanza 1.2.3\n");
    }

    #[test]
    fn version_line_with_commit() {
        assert_eq!(
            version_line("1.2.3", Some("abc1234")),
            "stanza 1.2.3 (abc1234)\n"
        );
    }

    #[test]
    fn version_matches_package_json() {
        let text = include_str!("../../../package.json");
        assert!(text.contains(&format!("\"version\": \"{VERSION}\"")));
    }

    #[test]
    fn usage_errors_exit_2() {
        let (stdout, stderr, code) = run_with(&["--check"], b"");
        assert_eq!((stdout.as_slice(), code), (&b""[..], 2));
        assert!(stderr.starts_with("stanza: nothing to format: give paths"));
    }

    struct Full;

    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn run_into(stdout: Box<dyn Write>) -> (String, i32) {
        let stderr = Captured::default();
        let mut io = Io {
            cwd: "/".to_owned(),
            stdin: Box::new(|| Ok(b"export {};\n".to_vec())),
            stdout,
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args = ["--fix", "--stdin", "x.ts"].map(str::to_owned);
        let code = run(&args, &mut io);
        (String::from_utf8(stderr.0.take()).unwrap(), code)
    }

    #[test]
    fn a_failed_write_fails_the_run_and_a_closed_pipe_does_not() {
        let (stderr, code) = run_into(Box::new(Full));
        assert_eq!(code, 1);
        assert!(
            stderr.starts_with("stanza: cannot write output: "),
            "{stderr}"
        );

        assert_eq!(run_into(Box::new(Closed)), (String::new(), 0));
    }

    #[test]
    fn fix_stdin_keeps_the_byte_order_mark() {
        let source = "\u{feff}let x = ;\n".as_bytes();
        let (stdout, stderr, code) = run_with(&["--fix", "--stdin", "x.ts"], source);
        assert_eq!(stdout, source);
        assert!(stderr.starts_with("x.ts:1:9 parse "), "{stderr}");
        assert_eq!(code, 2);
    }
}
