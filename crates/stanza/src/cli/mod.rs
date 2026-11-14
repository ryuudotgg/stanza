mod args;
mod explain;
mod hook;
mod io;
mod report;
mod run;

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;

use crate::engine::rules::RULES;
use crate::files::reason;
use crate::format::catch_panic;
use crate::hook::{CLAUDE_CODE_HOOKS, HookExit};
use crate::paths::{invalid_name, is_absolute};
use crate::usage::{FLAGS, USAGE, columns};
use args::Source;
use explain::run_explain;
use hook::run_hook;
use io::{Lost, Unwritten};
use run::{run_changed, run_paths, run_staged, run_stdin};

pub use args::{Command, command};
pub use io::{Io, Program};

pub const VERSION: &str = env!("STANZA_VERSION");

pub const COMMIT: Option<&str> = option_env!("STANZA_COMMIT");

pub fn working_directory() -> Result<String, String> {
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot read the working directory: {}", reason(&error)))?;

    cwd.into_os_string().into_string().map_err(|cwd| {
        format!(
            "cannot use the working directory, its {}",
            invalid_name(cwd.as_bytes())
        )
    })
}

pub fn version_line(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("stanza {version} ({commit})\n"),
        None => format!("stanza {version}\n"),
    }
}

fn help() -> String {
    let ids: Vec<String> = RULES
        .iter()
        .map(|entry| format!("  {}", entry.rule.id()))
        .collect();

    let rows: Vec<[&str; 3]> = RULES
        .iter()
        .zip(&ids)
        .map(|(entry, id)| {
            let kind = if entry.fixable { "fix" } else { "check" };
            [id.as_str(), kind, entry.summary]
        })
        .collect();

    [
        vec![USAGE.to_owned(), String::new()],
        columns(&FLAGS),
        vec![
            String::new(),
            "Claude Code .claude/settings.json:".to_owned(),
            CLAUDE_CODE_HOOKS.to_owned(),
            String::new(),
            "Rules:".to_owned(),
        ],
        columns(&rows),
    ]
    .concat()
    .join("\n")
}

pub fn run(argv: &[String], io: &mut Io) -> i32 {
    let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
    let command = command(&argv, io.agent_hooks.as_deref());
    execute(&command, io)
}

pub fn execute(command: &Command, io: &mut Io) -> i32 {
    let program = command.runs();
    let code = catch_panic(|| dispatch(command, io)).unwrap_or_else(|panic| {
        io.error(program, format_args!("internal error: {panic}"));
        program.failure_code()
    });

    let flushed = io.stdout.flush();
    io.record(flushed, Lost::Results);

    let Some(Unwritten { error, lost }) = &io.unwritten else {
        return code;
    };

    let line = Program::Stanza.line(format_args!("cannot write output: {}", reason(error)));
    let _ = writeln!(io.stderr, "{line}");
    match lost {
        Lost::Notice => code,
        Lost::Results => program.failure_code(),
    }
}

fn dispatch(command: &Command, io: &mut Io) -> i32 {
    match command {
        Command::Help => {
            io.out(format!("{}\n", help()).as_bytes());
            0
        }
        Command::Version => {
            io.out(version_line(VERSION, COMMIT).as_bytes());
            0
        }
        Command::HookOff => HookExit::Done.code(),
        Command::Usage { program, error } => {
            io.error(*program, format_args!("{error}\n{USAGE}"));
            program.failure_code()
        }
        Command::Hook(flags) => run_hook(flags, io).code(),
        Command::Explain(args) => run_explain(args, io),
        Command::Format(args) => match &args.source {
            Source::Paths { paths, invalid } => run_paths(paths, invalid, args, io),
            Source::Changed { hunks } => run_changed(*hunks, args, io),
            Source::Staged { hunks } => run_staged(*hunks, args, io),
            Source::Stdin(input) => run_stdin(input, args, io),
        },
    }
}

fn required_cwd(io: &mut Io) -> Option<String> {
    match io.cwd.clone() {
        Ok(cwd) => Some(cwd),
        Err(error) => {
            io.error(Program::Stanza, error);
            None
        }
    }
}

fn resolving_cwd<'a>(io: &mut Io, inputs: impl IntoIterator<Item = &'a str>) -> Option<String> {
    if inputs.into_iter().all(is_absolute) {
        return Some(io.cwd.clone().unwrap_or_else(|_| "/".to_owned()));
    }

    required_cwd(io)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::args::parse_arguments;
    use super::report::{FINDINGS_CHUNK, empty_message, write_findings};
    use super::run::{Finished, finish_files};
    use super::*;
    use crate::engine::rules::Rule;
    use crate::engine::types::{FileFindings, Finding, Located};
    use crate::files::{EmptyReason, realpath};
    use crate::format::{Findings, Formatted};
    use crate::paths::dirname;
    use crate::pool::STACK_LADDER;
    use crate::step::Unread;

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
            cwd: Ok(std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .to_string_lossy()
                .into_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
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

    fn check_with_workers(dir: &str, target: &str, workers: &str) -> (String, String, i32, usize) {
        run_with_workers("--check", dir, target, workers)
    }

    fn run_with_workers(
        mode: &str,
        dir: &str,
        target: &str,
        workers: &str,
    ) -> (String, String, i32, usize) {
        let (stdout, stderr) = (Captured::default(), Captured::default());
        let mut io = Io {
            cwd: Ok(dir.to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: Some(workers.to_owned()),
            agent_hooks: None,
            stdin: Box::new(|| Ok(Vec::new())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let spawned = crate::pool::SPAWNED.get();
        let code = run(&[mode.to_owned(), target.to_owned()], &mut io);
        let stdout = String::from_utf8(stdout.0.take()).unwrap();
        let stderr = String::from_utf8(stderr.0.take()).unwrap();
        (stdout, stderr, code, crate::pool::SPAWNED.get() - spawned)
    }

    #[test]
    fn stanza_workers_reaches_the_pool_and_output_matches_serial() {
        let dir = std::env::temp_dir().join(format!("stanza-workers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..crate::pool::CHUNK_SIZE * 3 {
            let source = if index % 50 == 0 {
                "if (a) {\n  b();\n}\n"
            } else {
                "export {};\n"
            };
            std::fs::write(dir.join(format!("{index:04}.ts")), source).unwrap();
        }

        let dir = realpath(&dir.to_string_lossy()).unwrap();
        let (stdout, stderr, code, spawned) = check_with_workers(&dir, ".", "0");
        assert_eq!((code, spawned, stderr.as_str()), (1, 0, ""));
        assert_eq!(stdout.lines().count(), 4, "{stdout}");
        assert_eq!(
            check_with_workers(&dir, ".", "2"),
            (stdout, stderr, code, 2)
        );

        let (one, _, _, _) = check_with_workers(&dir, "0000.ts", "0");
        assert_eq!(
            check_with_workers(&dir, "0000.ts", "2"),
            (one, String::new(), 1, 2)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pooled_fix_runs_on_workers_and_writes_what_a_serial_fix_writes() {
        let root = std::env::temp_dir().join(format!("stanza-pooled-fix-{}", std::process::id()));
        let trees = ["serial", "pooled"].map(|name| root.join(name));
        for tree in &trees {
            std::fs::create_dir_all(tree).unwrap();
            for index in 0..crate::pool::CHUNK_SIZE * 8 {
                let source = if index % 7 == 0 {
                    "if (a) {\n  b();\n}\nc();\n"
                } else {
                    "export {};\n"
                };
                std::fs::write(tree.join(format!("{index:04}.ts")), source).unwrap();
            }
        }

        let [serial, pooled] = trees.map(|tree| realpath(&tree.to_string_lossy()).unwrap());
        let (stdout, stderr, code, spawned) = run_with_workers("--fix", &serial, ".", "0");
        assert_eq!((code, spawned, stderr.as_str()), (0, 0, ""));
        assert_eq!(
            run_with_workers("--fix", &pooled, ".", "2"),
            (stdout, stderr, code, 2)
        );

        let files = |tree: &str| {
            let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(tree)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    (name, std::fs::read(entry.path()).unwrap())
                })
                .collect();
            files.sort();
            files
        };

        let fixed = files(&serial);
        assert_eq!(fixed.len(), crate::pool::CHUNK_SIZE * 8);
        assert_eq!(fixed[0].1, b"if (a) b();\nc();\n");
        assert_eq!(files(&pooled), fixed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_system_io_takes_stanza_workers_from_the_environment() {
        let workers = |name: &str| (name == "STANZA_WORKERS").then(|| OsString::from("64"));
        assert_eq!(
            Io::system(Ok("/".to_owned()), Some(STACK_LADDER[0]), workers)
                .workers
                .as_deref(),
            Some("64")
        );
        assert_eq!(
            Io::system(Ok("/".to_owned()), Some(STACK_LADDER[0]), |_| None).workers,
            None
        );
    }

    fn finished_with(pool_warning: Option<&str>) -> (String, String, i32) {
        let (stdout, stderr) = (Captured::default(), Captured::default());
        let mut io = Io {
            cwd: Ok("/".to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
            stdin: Box::new(|| Ok(Vec::new())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args = parse_arguments(&["--check".into(), ".".into()]).unwrap();
        let generated = empty_message(EmptyReason::Generated, &[".".to_owned()]);
        let finished = Finished {
            formatted: Formatted {
                findings: Findings::default(),
                failed: false,
                only_generated: true,
                kept: None,
                rewritten: Vec::new(),
                unread: Unread::default(),
                crashed: Vec::new(),
            },
            selection_errors: Vec::new(),
            pool_warning: pool_warning.map(str::to_owned),
            generated: Some(generated),
        };

        let code = finish_files(&args, &mut io, finished);
        let stdout = String::from_utf8(stdout.0.take()).unwrap();
        (stdout, String::from_utf8(stderr.0.take()).unwrap(), code)
    }

    #[test]
    fn a_worker_that_could_not_start_is_one_warning_and_keeps_the_exit_code() {
        assert_eq!(
            finished_with(Some("1 of 3 worker threads could not start")),
            (
                String::new(),
                "stanza: 1 of 3 worker threads could not start\nstanza: nothing to format in ., generated files are skipped\n".to_owned(),
                0
            )
        );

        assert_eq!(
            finished_with(None),
            (
                String::new(),
                "stanza: nothing to format in ., generated files are skipped\n".to_owned(),
                0
            )
        );
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
        let text = include_str!("../../../../package.json");
        assert!(text.contains(&format!("\"version\": \"{VERSION}\"")));
    }

    #[test]
    fn usage_errors_exit_2() {
        let (stdout, stderr, code) = run_with(&["--check"], b"");
        assert_eq!((stdout.as_slice(), code), (&b""[..], 2));
        assert!(stderr.starts_with("stanza: nothing to format: give paths"));
    }

    struct FillsUp {
        room: usize,
        written: Vec<u8>,
    }

    impl Write for FillsUp {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.room == 0 {
                return Err(io::Error::from_raw_os_error(libc::ENOSPC));
            }

            let taken = bytes.len().min(self.room);
            self.room -= taken;
            self.written.extend_from_slice(&bytes[..taken]);
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn streamed_findings_match_the_whole_report_and_stop_at_the_first_failed_write() {
        let findings = Findings::new(vec![FileFindings {
            path: "a.ts".to_owned(),
            findings: (1..=4000)
                .map(|line| Finding {
                    line,
                    col: 1,
                    rule: Rule::Wall,
                    message: "x".repeat(40),
                    fixable: false,
                })
                .collect(),
        }]);

        let located: Vec<Located> = findings.ordered().collect();
        let mut json = serde_json::to_vec(&located).unwrap();
        json.push(b'\n');
        let text: String = located
            .iter()
            .map(|located| format!("a.ts:{}:1 wall {}\n", located.finding.line, "x".repeat(40)))
            .collect();

        for (json_flag, expected) in [(false, text.into_bytes()), (true, json)] {
            let mut whole = Vec::new();
            write_findings(&mut whole, &findings, json_flag).unwrap();
            assert!(whole.len() > 2 * FINDINGS_CHUNK);
            assert_eq!(whole, expected);

            let mut out = FillsUp {
                room: FINDINGS_CHUNK * 3 / 2,
                written: Vec::new(),
            };

            let failed = write_findings(&mut out, &findings, json_flag).unwrap_err();
            assert_eq!(failed.raw_os_error(), Some(libc::ENOSPC));
            assert_eq!(out.written, expected[..FINDINGS_CHUNK * 3 / 2]);
        }

        let closed = write_findings(&mut Closed, &findings, true).unwrap_err();
        assert_eq!(closed.kind(), io::ErrorKind::BrokenPipe);
    }

    struct Unflushed;

    impl Write for Unflushed {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }
    }

    #[test]
    fn a_failed_flush_fails_only_a_running_hook_as_the_hook() {
        let flushed = |args: &[&str], agent_hooks: Option<&str>| {
            let mut io = Io {
                cwd: Ok("/".to_owned()),
                stack: Some(STACK_LADDER[0]),
                workers: None,
                agent_hooks: agent_hooks.map(str::to_owned),
                stdin: Box::new(|| Ok(b"{}".to_vec())),
                stdout: Box::new(Unflushed),
                stderr: Box::new(Captured::default()),
                unwritten: None,
            };

            let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
            run(&args, &mut io)
        };

        assert_eq!(flushed(&["hook"], Some("0")), 2);
        assert_eq!(flushed(&["hook", "--bogus"], None), 2);
        assert_eq!(flushed(&["hook"], None), 1);
        assert_eq!(flushed(&["--version"], None), 2);
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

    fn run_into(
        cwd: &str,
        args: &[&str],
        source: &str,
        stdout: Box<dyn Write>,
        stderr: Box<dyn Write>,
    ) -> i32 {
        let source = source.as_bytes().to_vec();
        let mut io = Io {
            cwd: Ok(cwd.to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
            stdin: Box::new(move || Ok(source.clone())),
            stdout,
            stderr,
            unwritten: None,
        };

        let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
        run(&args, &mut io)
    }

    #[test]
    fn a_failed_write_fails_the_run_and_a_closed_pipe_does_not() {
        let fix = ["--fix", "--stdin", "x.ts"];
        let stderr = Captured::default();
        let code = run_into("/", &fix, BRACED, Box::new(Full), Box::new(stderr.clone()));
        let stderr = String::from_utf8(stderr.0.take()).unwrap();
        assert_eq!(code, 2);
        assert!(
            stderr.starts_with("stanza: cannot write output: "),
            "{stderr}"
        );

        let fix_json = ["--fix", "--json", "--stdin", "x.ts"];
        let sink = || Box::new(Captured::default());
        assert_eq!(run_into("/", &fix_json, BRACED, sink(), Box::new(Full)), 2);

        let check = ["--check", "--stdin", "x.ts"];
        assert_eq!(run_into("/", &check, BRACED, Box::new(Full), sink()), 2);
        assert_eq!(run_into("/", &fix, BRACED, Box::new(Closed), sink()), 0);
        assert_eq!(run_into("/", &check, BRACED, Box::new(Closed), sink()), 1);
        let check_json = ["--check", "--json", "--stdin", "x.ts"];
        assert_eq!(
            run_into("/", &check_json, BRACED, Box::new(Closed), sink()),
            1
        );

        let dir = crash_tree("unwritten", &[], true);
        std::fs::write(format!("{dir}/.oxfmtrc.json"), "{").unwrap();
        let warned = Captured::default();
        run_into(&dir, &check, UNBRACED, sink(), Box::new(warned.clone()));
        assert!(!warned.0.take().is_empty());
        for (source, expected) in [(UNBRACED, 0), (BRACED, 1)] {
            let stdout = Captured::default();
            let code = run_into(
                &dir,
                &check,
                source,
                Box::new(stdout.clone()),
                Box::new(Full),
            );
            assert_eq!(code, expected);
            assert_eq!(stdout.0.take().is_empty(), expected == 0);
        }

        let write = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "cwd": dir,
            "tool_input": {"file_path": format!("{dir}/x.ts"), "content": BRACED},
        })
        .to_string();

        let hook = |stdout: Box<dyn Write>| {
            let event = write.clone().into_bytes();
            let mut io = Io {
                cwd: Ok(dir.clone()),
                stack: Some(STACK_LADDER[0]),
                workers: None,
                agent_hooks: None,
                stdin: Box::new(move || Ok(event.clone())),
                stdout,
                stderr: Box::new(Captured::default()),
                unwritten: None,
            };

            run(&["hook".to_owned()], &mut io)
        };

        assert_eq!(hook(Box::new(Full)), 1);
        assert_eq!(hook(Box::new(Closed)), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_hook_prints_config_notices_and_lost_output_as_stanza() {
        let dir = crash_tree("hook-notices", &[], true);
        std::fs::write(format!("{dir}/.oxfmtrc.json"), "{").unwrap();
        let write = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "cwd": dir,
            "tool_input": {"file_path": format!("{dir}/x.ts"), "content": BRACED},
        })
        .to_string();

        let hook = |stdout: Box<dyn Write>| {
            let event = write.clone().into_bytes();
            let stderr = Captured::default();
            let mut io = Io {
                cwd: Ok(dir.clone()),
                stack: Some(STACK_LADDER[0]),
                workers: None,
                agent_hooks: None,
                stdin: Box::new(move || Ok(event.clone())),
                stdout,
                stderr: Box::new(stderr.clone()),
                unwritten: None,
            };

            let code = run(&["hook".to_owned()], &mut io);
            (code, String::from_utf8(stderr.0.take()).unwrap())
        };

        let notice = "stanza: could not read the line width from .oxfmtrc.json, so stanza used the next source for it\n";
        assert_eq!(hook(Box::new(Closed)), (0, notice.to_owned()));

        let (code, stderr) = hook(Box::new(Full));
        assert_eq!(code, 1);
        let lost = stderr.strip_prefix(notice).unwrap_or_default();
        assert!(
            lost.starts_with("stanza: cannot write output: "),
            "{stderr}"
        );

        std::fs::write(format!("{dir}/x.ts"), BRACED).unwrap();
        let edit = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "cwd": dir,
            "tool_input": {"file_path": format!("{dir}/x.ts")},
        })
        .to_string()
        .into_bytes();

        let (_, stderr, code) = hook_in(&dir, edit);
        assert_eq!((stderr.as_str(), code), (notice, 0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn hook_in(dir: &str, stdin: Vec<u8>) -> (String, String, i32) {
        let (stdout, stderr) = (Captured::default(), Captured::default());
        let mut io = Io {
            cwd: Ok(dir.to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
            stdin: Box::new(move || Ok(stdin.clone())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let code = run(&["hook".to_owned()], &mut io);
        let stdout = String::from_utf8(stdout.0.take()).unwrap();
        (stdout, String::from_utf8(stderr.0.take()).unwrap(), code)
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    #[derive(serde::Deserialize)]
    struct WriteEvent<'a> {
        #[serde(borrow)]
        tool_input: &'a serde_json::value::RawValue,
    }

    fn recorded_content(stdout: &str) -> String {
        let (_, rest) = stdout.split_once(r#""content":"#).unwrap();
        serde::Deserialize::deserialize(&mut serde_json::Deserializer::from_str(rest)).unwrap()
    }

    // Rows outside this list keep the stdout the JavaScript hook recorded, which rewrote the echo.
    const STDOUT_AS_RECORDED: &[&str] = &[
        "plain",
        "literals",
        "file_path that needs escaping",
        "invalid utf-8",
        "lone surrogate in content",
        "lone surrogate in file_path",
        "tool input nested 40,000 deep",
    ];

    #[test]
    fn pre_tool_use_echoes_the_tool_input_as_sent_with_only_content_replaced() {
        let table: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/hook/updated-input.json"
        ))
        .unwrap();

        let original =
            serde_json::to_string("function f() {\n  const x = {\n    a,\n  };\n  log(x);\n}\n")
                .unwrap();
        let prefix = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","updatedInput":"#;
        let names: Vec<&str> = table
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert!(names.len() >= 18);
        for name in STDOUT_AS_RECORDED {
            assert!(names.contains(name), "{name}");
        }

        for (index, row) in table.iter().enumerate() {
            let name = names[index];
            let dir = std::env::temp_dir()
                .join(format!("stanza-hook-echo-{}-{index}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let dir = realpath(&dir.to_string_lossy()).unwrap();
            let init = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&dir)
                .status()
                .unwrap();

            assert!(init.success());

            let stdin = hex(row["stdin"].as_str().unwrap());
            let recorded = row["stdout"].as_str().unwrap();
            let (stdout, stderr, code) = hook_in(&dir, stdin.clone());
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!((stderr.as_str(), code), ("", 0), "{name}");
            if STDOUT_AS_RECORDED.contains(&name) {
                assert_eq!(stdout, recorded, "{name}");
            }

            if recorded.is_empty() {
                assert!(STDOUT_AS_RECORDED.contains(&name), "{name}");
                continue;
            }

            let text = String::from_utf8_lossy(&stdin);
            let sent = serde_json::from_str::<WriteEvent>(&text)
                .unwrap()
                .tool_input
                .get();
            assert_eq!(sent.matches(&original).count(), 1, "{name}");

            let formatted = serde_json::to_string(&recorded_content(recorded)).unwrap();
            let echo = sent.replacen(&original, &formatted, 1);
            let context = r#","additionalContext":"#;
            let (_, recorded_tail) = recorded.rsplit_once(context).unwrap();
            assert_eq!(
                stdout,
                [prefix, &echo, context, recorded_tail].concat(),
                "{name}"
            );
        }
    }

    #[test]
    fn agent_hooks_off_skips_flags_and_stdin() {
        let (stdout, stderr) = (Captured::default(), Captured::default());
        let mut io = Io {
            cwd: Ok("/".to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: Some("0".to_owned()),
            stdin: Box::new(|| Err(io::Error::from(io::ErrorKind::Other))),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args = ["hook", "--bogus"].map(str::to_owned);
        assert_eq!(run(&args, &mut io), 0);
        assert!(stdout.0.take().is_empty() && stderr.0.take().is_empty());
    }

    fn run_from(
        cwd: Result<&str, &str>,
        args: &[&str],
        stdin: impl FnMut() -> io::Result<Vec<u8>> + 'static,
    ) -> (String, String, i32) {
        let (stdout, stderr) = (Captured::default(), Captured::default());
        let mut io = Io {
            cwd: cwd.map(str::to_owned).map_err(str::to_owned),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
            stdin: Box::new(stdin),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
        let code = run(&args, &mut io);
        let stdout = String::from_utf8(stdout.0.take()).unwrap();
        (stdout, String::from_utf8(stderr.0.take()).unwrap(), code)
    }

    const UNREADABLE: &str = "cannot read the working directory: no such file or directory";

    #[test]
    fn only_commands_that_parse_source_ask_for_a_stack() {
        let parses = |argv: &[&str], agent_hooks: Option<&str>| {
            let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
            command(&argv, agent_hooks).parses_source()
        };

        for argv in [
            &["--version"][..],
            &["--help"],
            &["-h"],
            &[],
            &["--check"],
            &["--nope", "a.ts"],
        ] {
            assert!(!parses(argv, None), "{argv:?}");
        }

        for argv in [&["hook", "--bogus"][..], &["explain"], &["explain", "a.ts"]] {
            assert!(!parses(argv, None), "{argv:?}");
        }

        assert!(!parses(&["hook", "--bogus"], Some("0")));
        for argv in [
            &["hook"][..],
            &["explain", "a.ts:1"],
            &["--check", "a.ts"],
            &["--fix", "--stdin", "a.ts"],
        ] {
            assert!(parses(argv, None), "{argv:?}");
        }
    }

    #[test]
    fn an_unreadable_working_directory_fails_only_runs_that_resolve_against_it() {
        let dir = std::env::temp_dir().join(format!("stanza-no-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = realpath(&dir.to_string_lossy()).unwrap();
        let file = format!("{dir}/a.ts");
        std::fs::write(&file, BRACED).unwrap();

        let (stdout, stderr, code) =
            run_from(Err(UNREADABLE), &["--check", &file], || Ok(Vec::new()));
        assert!(stdout.starts_with(&format!("{file}:1:")), "{stdout}");
        assert_eq!((stderr.as_str(), code), ("", 1));

        let (stdout, _, code) = run_from(Err(UNREADABLE), &["--fix", "--stdin", &file], || {
            Ok(BRACED.as_bytes().to_vec())
        });
        assert_eq!((stdout.as_str(), code), (UNBRACED, 0));

        for args in [
            &["--check", "a.ts"][..],
            &["--check", &file, "a.ts"],
            &["--check", "--changed"],
            &["--check", "--staged"],
            &["--check", "--stdin", "a.ts"],
            &["explain", "a.ts:1"],
        ] {
            assert_eq!(
                run_from(Err(UNREADABLE), args, || Ok(Vec::new())),
                (String::new(), format!("stanza: {UNREADABLE}\n"), 2),
                "{args:?}"
            );
        }

        let edit = |cwd: &str| {
            let input = format!(
                r#"{{"hook_event_name":"PostToolUse",{cwd}"tool_name":"Edit","tool_input":{{"file_path":"{file}"}}}}"#
            );
            move || Ok(input.clone().into_bytes())
        };

        let absolute = format!(r#""cwd":"{dir}","#);
        assert_eq!(
            run_from(Err(UNREADABLE), &["hook"], edit(&absolute)),
            run_from(Ok("/"), &["hook"], edit(&absolute))
        );

        for cwd in [r#""cwd":"sub","#, ""] {
            assert_eq!(
                run_from(Err(UNREADABLE), &["hook"], edit(cwd)),
                (String::new(), format!("stanza hook: {UNREADABLE}\n"), 1),
                "{cwd}"
            );
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_panic_that_escapes_the_run_is_one_line() {
        let boom = || -> io::Result<Vec<u8>> { panic!("boom\n  twice") };
        assert_eq!(
            run_from(Ok("/"), &["hook"], boom),
            (
                String::new(),
                "stanza hook: internal error: boom twice\n".to_owned(),
                1
            )
        );
        assert_eq!(
            run_from(Ok("/"), &["--check", "--stdin", "/a.ts"], boom),
            (
                String::new(),
                "stanza: internal error: boom twice\n".to_owned(),
                2
            )
        );
    }

    const BRACED: &str = "if (a) {\n  b();\n}\nc();\n";

    const UNBRACED: &str = "if (a) b();\nc();\n";

    const CRASHED: &str = "panics.ts:1:1 error internal error (injected panic), left untouched";

    const CRASH_LINE: &str = "internal error on panics.ts, left it untouched: injected panic\n";

    fn crash_tree(name: &str, files: &[&str], git: bool) -> String {
        let dir = std::env::temp_dir().join(format!("stanza-crash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), BRACED).unwrap();
        }

        let dir = realpath(&dir.to_string_lossy()).unwrap();
        if git {
            git_in(&dir, &["init", "-q"]);
        }

        dir
    }

    fn git_in(dir: &str, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();

        assert!(status.success());
    }

    fn run_at(dir: &str, args: &[&str], workers: &str) -> (String, String, i32) {
        let (stdout, stderr) = (Captured::default(), Captured::default());
        let mut io = Io {
            cwd: Ok(dir.to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: Some(workers.to_owned()),
            agent_hooks: None,
            stdin: Box::new(|| Ok(Vec::new())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            unwritten: None,
        };

        let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
        let code = run(&args, &mut io);
        let stdout = String::from_utf8(stdout.0.take()).unwrap();
        (stdout, String::from_utf8(stderr.0.take()).unwrap(), code)
    }

    fn read_in(dir: &str, file: &str) -> String {
        std::fs::read_to_string(format!("{dir}/{file}")).unwrap()
    }

    #[test]
    fn a_panic_on_one_file_is_an_error_finding_and_the_other_files_still_run() {
        let dir = crash_tree("serial", &["a.ts", "panics.ts", "z.ts"], false);
        let (stdout, stderr, code) = run_at(&dir, &["--check", "."], "0");
        assert_eq!(
            (stderr.as_str(), code),
            (&*format!("stanza: {CRASH_LINE}"), 2)
        );
        assert!(
            stdout.lines().any(|line| line.starts_with("a.ts:")),
            "{stdout}"
        );
        assert!(
            stdout.lines().any(|line| line.starts_with("z.ts:")),
            "{stdout}"
        );
        assert_eq!(
            stdout
                .lines()
                .filter(|line| line.starts_with("panics.ts"))
                .collect::<Vec<_>>(),
            [CRASHED]
        );

        let (stdout, _, _) = run_at(&dir, &["--check", "--json", "."], "0");
        let findings: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        let crashed: Vec<_> = findings
            .iter()
            .filter(|finding| finding["path"] == "panics.ts")
            .collect();

        assert_eq!(crashed.len(), 1);
        assert_eq!(crashed[0]["rule"], "error");

        let (stdout, stderr, code) = run_at(&dir, &["--fix", "."], "0");
        assert_eq!(
            (stdout.as_str(), stderr.as_str(), code),
            (
                &*format!("{CRASHED}\n"),
                &*format!("stanza: {CRASH_LINE}"),
                2
            )
        );

        assert_eq!(read_in(&dir, "a.ts"), UNBRACED);
        assert_eq!(read_in(&dir, "z.ts"), UNBRACED);
        assert_eq!(read_in(&dir, "panics.ts"), BRACED);
        std::fs::remove_dir_all(dir).unwrap();
    }

    const PANICKING_CONFIG: &str =
        "import level from \"./panics.ts\";\nexport default [{ rules: { curly: level } }];\n";

    fn config_crash_tree(name: &str, files: &[&str]) -> String {
        let dir = crash_tree(name, &[], false);
        std::fs::create_dir_all(format!("{dir}/broken")).unwrap();
        std::fs::write(format!("{dir}/broken/eslint.config.js"), PANICKING_CONFIG).unwrap();
        for file in std::iter::once(&"broken/panics.ts").chain(files) {
            let path = format!("{dir}/{file}");
            std::fs::create_dir_all(dirname(&path)).unwrap();
            std::fs::write(path, BRACED).unwrap();
        }

        dir
    }

    fn crashed_line(path: &str) -> String {
        format!("{path}:1:1 error internal error (injected panic), left untouched")
    }

    #[test]
    fn a_panic_evaluating_a_config_errors_only_the_files_that_need_it() {
        let targets = ["broken/a.ts", "broken/sub/b.ts", "fine/c.ts", "z.ts"];
        for workers in ["4", "0"] {
            let dir = config_crash_tree(&format!("config-{workers}"), &targets);
            let (stdout, stderr, code) =
                run_at(&dir, &[&["--check"], &targets[..]].concat(), workers);
            assert_eq!(
                (stderr.as_str(), code),
                (
                    "stanza: internal error on broken/a.ts, left it untouched: injected panic\nstanza: internal error on broken/sub/b.ts, left it untouched: injected panic\n",
                    2
                ),
                "{workers}"
            );

            for path in ["broken/a.ts", "broken/sub/b.ts"] {
                let lines: Vec<&str> = stdout
                    .lines()
                    .filter(|line| line.starts_with(path))
                    .collect();
                assert_eq!(lines, [crashed_line(path)], "{workers}");
            }

            for path in ["fine/c.ts", "z.ts"] {
                assert!(
                    stdout
                        .lines()
                        .any(|line| line.starts_with(&format!("{path}:"))
                            && !line.contains("internal error")),
                    "{workers} {stdout}"
                );
            }

            let (_, _, code) = run_at(&dir, &[&["--fix"], &targets[..]].concat(), workers);
            assert_eq!(code, 2);
            assert_eq!(read_in(&dir, "broken/a.ts"), BRACED);
            assert_eq!(read_in(&dir, "broken/sub/b.ts"), BRACED);
            assert_eq!(read_in(&dir, "fine/c.ts"), UNBRACED);
            assert_eq!(read_in(&dir, "z.ts"), UNBRACED);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_config_no_file_needs_is_never_evaluated() {
        let targets = ["broken/a.ts", "z.ts"];
        let dir = config_crash_tree("unneeded", &targets);
        let (stdout, stderr, code) = run_at(
            &dir,
            &[&["--check", "--no-braces"], &targets[..]].concat(),
            "4",
        );

        assert_eq!((stderr.as_str(), code), ("", 1), "{stdout}");
        assert!(!stdout.contains("internal error"), "{stdout}");
        assert_eq!(
            crate::languages::javascript::config::module::parses(&format!(
                "{dir}/broken/eslint.config.js"
            )),
            0
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_panic_evaluating_a_width_config_errors_the_files_it_sizes() {
        for workers in ["4", "0"] {
            let dir = config_crash_tree(&format!("width-{workers}"), &["wide/d.ts", "z.ts"]);
            std::fs::write(
                format!("{dir}/wide/prettier.config.js"),
                "import width from \"../broken/panics.ts\";\nexport default { printWidth: width };\n",
            )
            .unwrap();

            let (stdout, stderr, _) = run_at(
                &dir,
                &["--check", "--no-braces", "wide/d.ts", "z.ts"],
                workers,
            );
            assert_eq!(
                stderr, "stanza: internal error on wide/d.ts, left it untouched: injected panic\n",
                "{workers}"
            );
            let crashed: Vec<&str> = stdout
                .lines()
                .filter(|line| line.contains("internal error"))
                .collect();
            assert_eq!(crashed, [crashed_line("wide/d.ts")], "{workers}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    fn overridden_tree(name: &str, targets: &[String]) -> String {
        let dir = crash_tree(name, &[], false);
        std::fs::write(
            format!("{dir}/eslint.config.js"),
            "export default [{ rules: { curly: \"error\" } }, { files: [\"off/**\"], rules: { curly: \"off\" } }];\n",
        )
        .unwrap();

        std::fs::write(
            format!("{dir}/.prettierrc.json"),
            r#"{ "printWidth": 100, "overrides": [{ "files": "*.narrow.ts", "options": { "printWidth": 40 } }] }"#,
        )
        .unwrap();

        for target in targets {
            let path = format!("{dir}/{target}");
            std::fs::create_dir_all(dirname(&path)).unwrap();
            std::fs::write(
                path,
                "if (condition) {\n  doSomethingWithAVeryLongName(argumentOne, argumentTwo);\n}\nnext();\n",
            )
            .unwrap();
        }

        dir
    }

    #[test]
    fn workers_sharing_valid_configs_resolve_each_file_as_a_serial_run_does() {
        let targets: Vec<String> = (0..300)
            .map(|index| {
                let dir = if index % 2 == 0 { "" } else { "off/" };
                let name = if index % 3 == 0 { "narrow.ts" } else { "ts" };
                format!("{dir}{index:03}.{name}")
            })
            .collect();

        let checked = |dir: &str, workers: &str| {
            let args: Vec<&str> = std::iter::once("--check")
                .chain(targets.iter().map(String::as_str))
                .collect();
            run_at(dir, &args, workers).0
        };

        let pooled_dir = overridden_tree("valid-pooled", &targets);
        let serial_dir = overridden_tree("valid-serial", &targets);
        let pooled = checked(&pooled_dir, "4");
        assert_eq!(pooled, checked(&serial_dir, "0"));

        let rules = |path: &str| -> Vec<&str> {
            pooled
                .lines()
                .filter(|line| line.starts_with(&format!("{path}:")))
                .filter_map(|line| line.split_whitespace().nth(1))
                .collect()
        };

        assert_eq!(rules("000.narrow.ts"), ["after-multiline"]);
        assert_eq!(rules("002.ts"), ["after-multiline"]);
        assert_eq!(rules("off/003.narrow.ts"), ["braces", "after-multiline"]);
        assert_eq!(rules("off/001.ts"), ["braces"]);
        for config in ["eslint.config.js", ".prettierrc.json"] {
            assert_eq!(
                crate::languages::javascript::config::module::parses(&format!(
                    "{pooled_dir}/{config}"
                )),
                1,
                "{config}"
            );
        }

        std::fs::remove_dir_all(pooled_dir).unwrap();
        std::fs::remove_dir_all(serial_dir).unwrap();
    }

    #[test]
    fn every_worker_replays_a_config_panic_and_parses_the_config_once() {
        let targets: Vec<String> = (0..300)
            .map(|index| match index % 2 {
                0 => format!("broken/s{}/{index:03}.ts", index % 10),
                _ => format!("fine/{index:03}.ts"),
            })
            .collect();

        let files: Vec<&str> = targets.iter().map(String::as_str).collect();
        let dir = config_crash_tree("config-pool", &files);
        let args: Vec<&str> = std::iter::once("--check")
            .chain(files.iter().copied())
            .collect();
        let (stdout, stderr, code) = run_at(&dir, &args, "4");
        assert_eq!(code, 2);
        assert_eq!(stderr.lines().count(), 150, "{stderr}");
        for path in &targets {
            let crashed = stdout.lines().any(|line| {
                line.starts_with(&format!("{path}:")) && line.contains("injected panic")
            });
            assert_eq!(crashed, path.starts_with("broken"), "{path}");
        }

        assert_eq!(
            crate::languages::javascript::config::module::parses(&format!(
                "{dir}/broken/eslint.config.js"
            )),
            1
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pooled_run_reports_a_panic_as_the_serial_run_does() {
        let names: Vec<String> = (0..300)
            .map(|index| match index {
                149 => "0149-panics.ts".to_owned(),
                _ => format!("{index:04}.ts"),
            })
            .collect();

        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let trees = ["unpooled", "pooled"].map(|name| crash_tree(name, &names, false));
        for mode in ["--check", "--fix"] {
            let serial = run_with_workers(mode, &trees[0], ".", "0");
            let pooled = run_with_workers(mode, &trees[1], ".", "2");
            assert_eq!(pooled, (serial.0.clone(), serial.1.clone(), 2, 2), "{mode}");
            assert_eq!(
                serial.1,
                format!("stanza: {}", CRASH_LINE.replace("panics", "0149-panics"))
            );
        }

        for tree in &trees {
            assert_eq!(read_in(tree, "0149-panics.ts"), BRACED);
            assert_eq!(read_in(tree, "0148.ts"), UNBRACED);
            assert_eq!(read_in(tree, "0150.ts"), UNBRACED);
            std::fs::remove_dir_all(tree).unwrap();
        }
    }

    #[test]
    fn changed_and_staged_runs_report_a_panic_and_go_on() {
        let dir = crash_tree("changed", &["a.ts", "panics.ts", "z.ts"], true);
        let (stdout, stderr, code) = run_at(&dir, &["--fix", "--changed"], "0");
        assert_eq!(
            (stdout.as_str(), stderr.as_str(), code),
            (
                &*format!("{CRASHED}\n"),
                &*format!("stanza: {CRASH_LINE}"),
                2
            )
        );

        assert_eq!(read_in(&dir, "a.ts"), UNBRACED);
        assert_eq!(read_in(&dir, "panics.ts"), BRACED);

        for file in ["a.ts", "z.ts"] {
            std::fs::write(format!("{dir}/{file}"), BRACED).unwrap();
        }

        git_in(&dir, &["add", "."]);
        let (stdout, stderr, code) = run_at(&dir, &["--check", "--staged"], "0");
        assert_eq!(code, 2);
        assert!(
            stderr.starts_with(&format!("stanza: {CRASH_LINE}")),
            "{stderr}"
        );
        for file in ["a.ts:", "z.ts:", CRASHED] {
            assert!(
                stdout.lines().any(|line| line.starts_with(file)),
                "{stdout}"
            );
        }

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fix_stdin_passes_a_file_it_panics_on_through() {
        let (stdout, stderr, code) =
            run_with(&["--fix", "--stdin", "panics.ts"], BRACED.as_bytes());
        assert_eq!(stdout, BRACED.as_bytes());
        assert_eq!(
            (stderr.as_str(), code),
            (&*format!("{CRASHED}\nstanza: {CRASH_LINE}"), 2)
        );

        let (stdout, stderr, code) = run_with(
            &["--fix", "--json", "--stdin", "panics.ts"],
            BRACED.as_bytes(),
        );
        let findings: Vec<serde_json::Value> = serde_json::from_str(&stderr).unwrap();
        assert_eq!((stdout.as_slice(), code), (BRACED.as_bytes(), 2));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["rule"], "error");
    }

    #[test]
    fn every_hook_event_reports_a_panic_and_exits_1() {
        let dir = crash_tree("hook", &["a.ts", "panics.ts", "z.ts"], true);
        let stop = format!(r#"{{"hook_event_name":"Stop","cwd":"{dir}"}}"#);
        let (stdout, stderr, code) = hook_in(&dir, stop.into_bytes());
        assert_eq!(
            (stderr.as_str(), code),
            (&*format!("stanza hook: {CRASH_LINE}"), 1)
        );
        let reason: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert!(
            reason["reason"].as_str().unwrap().contains(
                "panics.ts:1:1 stanza failed on this file: internal error (injected panic), left untouched"
            ),
            "{stdout}"
        );

        assert_eq!(read_in(&dir, "a.ts"), UNBRACED);
        assert_eq!(read_in(&dir, "z.ts"), UNBRACED);
        assert_eq!(read_in(&dir, "panics.ts"), BRACED);

        let edit = format!(
            r#"{{"hook_event_name":"PostToolUse","tool_name":"Edit","cwd":"{dir}","tool_input":{{"file_path":"{dir}/panics.ts"}}}}"#
        );
        let write = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "cwd": dir,
            "tool_input": {"file_path": format!("{dir}/panics.ts"), "content": BRACED},
        });

        for event in [edit, write.to_string()] {
            assert_eq!(
                hook_in(&dir, event.into_bytes()),
                (String::new(), format!("stanza hook: {CRASH_LINE}"), 1)
            );
        }

        assert_eq!(read_in(&dir, "panics.ts"), BRACED);
        std::fs::remove_dir_all(dir).unwrap();
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
