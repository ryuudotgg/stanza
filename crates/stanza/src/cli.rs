use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};

use crate::engine::explain::{Request, explain};
use crate::engine::types::{Braces, Finding, Mode};
use crate::files::{
    EmptyReason, Repositories, SkipReason, StagedFile, StdinTarget, collect_changed, collect_files,
    collect_staged, empty_reason, error_code, is_generated_header, realpath, stdin_target,
};
use crate::format::{Formatted, Input, Run, format_inputs, read_text};
use crate::paths::{relative, resolve};
use crate::step::{Unread, decode, without_mark};
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
    hunks: bool,
    staged: bool,
    json: bool,
    braces: Option<Braces>,
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
        hunks,
        staged,
        json: seen.contains("--json"),
        braces: if seen.contains("--braces") {
            Some(Braces::On)
        } else if seen.contains("--no-braces") {
            Some(Braces::Off)
        } else {
            None
        },
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

type Notice = fn(&str) -> String;

fn warn_unread(io: &mut Io, unread: &Unread) {
    let notices: [(&[String], Notice); 2] = [
        (&unread.braces, |files| {
            format!(
                "stanza: could not tell whether {files} enforces braces, so braces stay; pass --braces or --no-braces to settle it"
            )
        }),
        (&unread.width, |files| {
            format!(
                "stanza: could not read the line width from {files}, so stanza used the next source for it"
            )
        }),
    ];

    for (paths, message) in notices {
        if paths.is_empty() {
            continue;
        }

        let unique: BTreeSet<&String> = paths.iter().collect();
        let mut files: Vec<String> = unique
            .into_iter()
            .map(|path| printed_path(path, &io.cwd))
            .collect();

        files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        io.warn(&message(&files.join(", ")));
    }
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

struct ExplainArguments {
    path: String,
    line: f64,
    no_braces: bool,
}

fn location(target: &str) -> Option<(&str, &str)> {
    let (path, line) = target.rsplit_once(':')?;
    let terminated = path.contains(['\n', '\r', '\u{2028}', '\u{2029}']);
    let digits = !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit());
    (!path.is_empty() && !terminated && digits).then_some((path, line))
}

fn parse_explain(args: &[String]) -> Result<ExplainArguments, String> {
    let (option_args, literal) = match args.iter().position(|arg| arg == "--") {
        Some(delimiter) => (&args[..delimiter], &args[delimiter + 1..]),
        None => (args, &args[..0]),
    };

    let option_targets: Vec<&String> = option_args
        .iter()
        .filter(|arg| *arg != "--no-braces")
        .collect();

    let no_braces = option_args.len() - option_targets.len();
    if no_braces > 1 {
        return Err("--no-braces given twice".to_owned());
    }

    let targets: Vec<&String> = option_targets.iter().copied().chain(literal).collect();
    let extra = (targets.len() > 1).then(|| {
        targets
            .iter()
            .find(|arg| **arg == "--")
            .unwrap_or(&targets[1])
    });

    let unexpected = option_targets
        .iter()
        .find(|arg| arg.starts_with('-'))
        .or(extra);

    if let Some(unexpected) = unexpected {
        return Err(format!("unexpected argument {unexpected}"));
    }

    let target = targets.first().map_or("", |target| target.as_str());
    let Some((path, digits)) = location(target) else {
        return Err("explain needs <file>:<line>".to_owned());
    };

    let line: f64 = digits.parse().expect("ASCII digits parse as a number");
    if line < 1.0 {
        return Err(format!("{target}: the line must be 1 or more"));
    }

    Ok(ExplainArguments {
        path: path.to_owned(),
        line,
        no_braces: no_braces > 0,
    })
}

fn run_explain(argv: &[String], io: &mut Io) -> i32 {
    let args = match parse_explain(argv) {
        Ok(args) => args,
        Err(error) => {
            io.warn(&format!("stanza: {error}\n{USAGE}"));
            return 2;
        }
    };

    let target = stdin_target(&args.path, &io.cwd, &mut Repositories::default());
    if let StdinTarget::Unsupported(error) | StdinTarget::Failed(error) = &target {
        io.warn(&format!("stanza: {error}"));
        return 2;
    }

    let supplied = resolve(&io.cwd, &args.path);
    let bytes = match std::fs::read(&supplied) {
        Ok(bytes) => bytes,
        Err(error) => {
            let failure = match error_code(&error) {
                Some("ENOENT") => format!("no such file: {}", args.path),
                code => format!(
                    "cannot read {}: {}",
                    args.path,
                    code.map_or_else(|| error.to_string(), str::to_owned)
                ),
            };

            io.warn(&format!("stanza: {failure}"));
            return 2;
        }
    };

    let text = match decode(bytes) {
        Ok(text) => text,
        Err(message) => {
            io.warn(&format!("stanza: {}: {message}", args.path));
            return 2;
        }
    };

    let (root, path) = match (realpath(&io.cwd), realpath(&supplied)) {
        (Ok(root), Ok(path)) => (root, path),
        (Err(error), _) | (_, Err(error)) => {
            io.warn(&format!("stanza: {error}"));
            return 2;
        }
    };

    let body = without_mark(&text);
    let display = |path: &str| printed_path(path, &root);
    let result = explain(&Request {
        path: &path,
        text: body,
        line: args.line,
        no_braces: args.no_braces,
        display: &display,
    });

    let explained = match result {
        Ok(explained) => explained,
        Err(error) => {
            io.warn(&format!("stanza: {error}"));
            return 2;
        }
    };

    for line in &explained.lines {
        io.out(format!("{line}\n").as_bytes());
    }

    if target == StdinTarget::Skip(SkipReason::OptedOut) {
        io.out(b"\nnote: --fix and --check skip this file, its stanza attribute opts it out\n");
    } else if matches!(target, StdinTarget::Skip(_)) || is_generated_header(body) {
        io.out(b"\nnote: --fix and --check skip this file as generated or excluded\n");
    }

    if explained.found { 0 } else { 1 }
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

    let target = stdin_target(input, &io.cwd, &mut Repositories::default());
    let inputs = match &target {
        StdinTarget::Format { path, .. } => vec![Input {
            path: path.clone(),
            output: printed_path(path, &io.cwd),
            text: decode(source.clone()),
        }],
        _ => Vec::new(),
    };

    let run = Run {
        mode: args.mode,
        braces: args.braces,
        write: false,
    };

    let formatted = format_inputs(inputs.into_iter(), &run);
    let printed = printed_findings(&formatted.findings, args.json);
    if args.mode == Mode::Fix {
        io.out(
            formatted
                .kept
                .as_ref()
                .map_or(&source[..], String::as_bytes),
        );
        io.err(&printed);
    } else {
        io.out(&printed);
    }

    if !(args.mode == Mode::Fix && args.json) {
        warn_unread(io, &formatted.unread);
    }

    let errors = match target {
        StdinTarget::Unsupported(error) | StdinTarget::Failed(error) => vec![error],
        StdinTarget::Format { .. } | StdinTarget::Skip(_) => Vec::new(),
    };

    for error in &errors {
        io.warn(&format!("stanza: {error}"));
    }

    status(&errors, &formatted)
}

fn empty_message(reason: EmptyReason, inputs: &[String]) -> String {
    let paths = inputs.join(", ");
    match reason {
        EmptyReason::OptedOut => {
            format!("stanza: nothing to format in {paths}, the stanza attribute opts its files out")
        }
        EmptyReason::Ignored => {
            format!("stanza: nothing to format in {paths}, git ignores its files")
        }
        EmptyReason::Generated => {
            format!("stanza: nothing to format in {paths}, generated files are skipped")
        }
        EmptyReason::Skipped => {
            format!("stanza: nothing to format in {paths}, its files are always skipped")
        }
        EmptyReason::None => format!("stanza: no TypeScript or JavaScript files under {paths}"),
    }
}

fn warn_submodules(io: &mut Io, submodules: &[String]) {
    for path in submodules {
        let shown = printed_path(path, &io.cwd);
        io.warn(&format!(
            "stanza: skipped git submodule {shown}, pass its path to format its files"
        ));
    }
}

fn run_files(args: &Arguments, io: &mut Io) -> i32 {
    let mut repositories = Repositories::default();
    let selected = if args.changed {
        let location = repositories.locate(&io.cwd);
        collect_changed(location, &mut repositories, None).map(|selected| (selected, None))
    } else {
        collect_files(&args.paths, &io.cwd, false, &mut repositories).and_then(|selected| {
            if !selected.files.is_empty() || !selected.errors.is_empty() {
                return Ok((selected, None));
            }

            if selected.opted_out {
                return Ok((selected, Some(EmptyReason::OptedOut)));
            }

            let submodules = selected.submodules.iter().cloned().collect();
            let reason = empty_reason(&args.paths, &io.cwd, &submodules, &mut repositories)?;
            let reason =
                (reason != EmptyReason::None || selected.submodules.is_empty()).then_some(reason);
            Ok((selected, reason))
        })
    };

    let (collected, reason) = match selected {
        Ok(selected) => selected,
        Err(error) => {
            io.warn(&format!("stanza: file selection failed: {error}"));
            return 2;
        }
    };

    let explains_generated = !args.changed && !collected.opted_out && collected.errors.is_empty();
    for warning in &collected.warnings {
        io.warn(&format!("stanza: {warning}"));
    }

    warn_submodules(io, &collected.submodules);
    if let Some(reason) = reason {
        io.warn(&empty_message(reason, &args.paths));
    }

    let cwd = io.cwd.clone();
    let run = Run {
        mode: args.mode,
        braces: args.braces,
        write: true,
    };

    let inputs = collected.files.into_iter().map(|path| Input {
        output: printed_path(&path, &cwd),
        text: read_text(&path),
        path,
    });

    let formatted = format_inputs(inputs, &run);

    if explains_generated && formatted.only_generated {
        io.warn(&empty_message(EmptyReason::Generated, &args.paths));
    }

    io.out(&printed_findings(&formatted.findings, args.json));

    for error in &collected.errors {
        io.warn(&format!("stanza: {error}"));
    }

    warn_unread(io, &formatted.unread);
    status(&collected.errors, &formatted)
}

fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+:,./-".contains(&byte));

    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn repair_lines(
    findings: &[Finding],
    files: &[StagedFile],
    cwd: &str,
    braces: Option<Braces>,
) -> Vec<String> {
    let fixable: HashSet<&str> = findings
        .iter()
        .filter(|finding| finding.fixable)
        .map(|finding| finding.path.as_str())
        .collect();

    let (dirty, clean): (Vec<&StagedFile>, Vec<&StagedFile>) = files
        .iter()
        .filter(|file| fixable.contains(printed_path(&file.path, cwd).as_str()))
        .partition(|file| file.unstaged);

    let names = |list: &[&StagedFile]| {
        list.iter()
            .map(|file| shell_word(&printed_path(&file.path, cwd)))
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut lines = Vec::new();
    if !dirty.is_empty() {
        lines.push(format!(
            "these also have unstaged changes, fix them with --fix and restage by hand: {}",
            names(&dirty)
        ));
    }

    if !clean.is_empty() {
        let flags = match braces {
            None => "",
            Some(Braces::On) => " --braces",
            Some(Braces::Off) => " --no-braces",
        };

        let names = names(&clean);
        lines.push(format!(
            "fix and restage with: cd {} && stanza --fix{flags} -- {names} && git --literal-pathspecs add -- {names}",
            shell_word(cwd)
        ));
    }

    lines
}

fn run_staged(args: &Arguments, io: &mut Io) -> i32 {
    let collected = collect_staged(&io.cwd, &mut Repositories::default());
    let files = match &collected {
        Ok(staged) => {
            warn_submodules(io, &staged.submodules);
            staged.files.as_slice()
        }
        Err(_) => &[],
    };

    let cwd = io.cwd.clone();
    let inputs = files.iter().map(|file| Input {
        path: file.path.clone(),
        output: printed_path(&file.path, &cwd),
        text: decode(file.bytes.clone()),
    });

    let run = Run {
        mode: args.mode,
        braces: args.braces,
        write: false,
    };

    let formatted = format_inputs(inputs, &run);
    io.out(&printed_findings(&formatted.findings, args.json));

    let errors = match &collected {
        Ok(_) => {
            for line in repair_lines(&formatted.findings, files, &cwd, args.braces) {
                io.warn(&line);
            }

            Vec::new()
        }
        Err(error) => {
            io.warn(&format!("stanza: {error}"));
            vec![error.clone()]
        }
    };

    warn_unread(io, &formatted.unread);
    status(&errors, &formatted)
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
        Some("explain") => return run_explain(&argv[1..], io),
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

    if args.hunks {
        return unsupported(io, "--hunks");
    }

    if args.staged {
        return run_staged(&args, io);
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
