use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{self, Read, Write};

use indexmap::{IndexMap, IndexSet};

use crate::engine::explain::{Request, explain};
use crate::engine::rules::RULES;
use crate::engine::types::{Braces, Finding, Mode};
use crate::files::{
    EmptyReason, Location, Repositories, SkipReason, StagedFile, StdinTarget, collect_changed,
    collect_files, collect_staged, empty_reason, error_code, ignored_by_git, is_generated_header,
    lands_within, prefetch_changed, realpath, stdin_target, tracked_in_head,
};
use crate::format::{
    Formatted, Input, Run, catch_panic, format_file, format_inputs, hard_linked, merge_outcomes,
    read_text,
};
use crate::hook::{
    CLAUDE_CODE_HOOKS, HookCall, HookExit, HookFlags, ToolInput, block_reason, hook_call,
    hook_flags, hook_input, reread_line,
};
use crate::json::{JsString, Json, concat, object, stringify, text};
use crate::paths::{dirname, is_absolute, relative, resolve};

use crate::pool::{DEFAULT_WORKERS, cores, run_pooled, worker_count};
use crate::step::{Unread, decode, without_mark};
use crate::usage::{BOTH_BRACES, FLAGS, USAGE, columns};

pub const VERSION: &str = env!("STANZA_VERSION");

pub const COMMIT: Option<&str> = option_env!("STANZA_COMMIT");

pub struct Io {
    pub cwd: Result<String, String>,
    pub stack: Option<usize>,
    pub workers: Option<String>,
    pub agent_hooks: Option<String>,
    pub stdin: Box<dyn FnMut() -> io::Result<Vec<u8>>>,
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    pub unwritten: Option<io::Error>,
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

pub fn working_directory() -> Result<String, String> {
    std::env::current_dir()
        .map(|cwd| cwd.to_string_lossy().into_owned())
        .map_err(|error| {
            let code = error_code(&error).map_or_else(|| error.to_string(), str::to_owned);
            format!("cannot read the working directory: {code}")
        })
}

pub fn version_line(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("stanza {version} ({commit})\n"),
        None => format!("stanza {version}\n"),
    }
}

pub struct Arguments {
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

fn printed_path(path: &str, cwd: Option<&str>) -> String {
    match cwd.map(|cwd| relative(cwd, path)) {
        Some(output) if !output.is_empty() => output,
        _ => path.to_owned(),
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

fn unread_lines(cwd: Option<&str>, unread: &Unread) -> Vec<String> {
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

    let mut lines = Vec::new();
    for (paths, message) in notices {
        if paths.is_empty() {
            continue;
        }

        let unique: BTreeSet<&String> = paths.iter().collect();
        let mut files: Vec<String> = unique
            .into_iter()
            .map(|path| printed_path(path, cwd))
            .collect();

        files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        lines.push(message(&files.join(", ")));
    }

    lines
}

fn warn_unread(io: &mut Io, unread: &Unread) {
    for line in unread_lines(io.cwd.as_deref().ok(), unread) {
        io.warn(&line);
    }
}

fn warn_crashed(io: &mut Io, prefix: &str, crashed: &[String]) {
    for line in crashed {
        io.warn(&format!("{prefix}: {line}"));
    }
}

fn status(errors: &[String], formatted: &Formatted) -> i32 {
    if !errors.is_empty() || formatted.failed {
        return 2;
    }

    i32::from(!formatted.findings.is_empty())
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

pub struct ExplainArguments {
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

fn run_explain(args: &ExplainArguments, io: &mut Io) -> i32 {
    let Some(cwd) = required_cwd(io) else {
        return 2;
    };

    let target = stdin_target(&args.path, &cwd, &mut Repositories::default());
    if let StdinTarget::Unsupported(error)
    | StdinTarget::Unreadable(error)
    | StdinTarget::Failed(error) = &target
    {
        io.warn(&format!("stanza: {error}"));
        return 2;
    }

    let supplied = resolve(&cwd, &args.path);
    let regular = std::fs::metadata(&supplied).and_then(|meta| {
        if meta.is_file() {
            std::fs::read(&supplied).map(Some)
        } else {
            Ok(None)
        }
    });

    let bytes = match regular {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            io.warn(&format!("stanza: not a regular file: {}", args.path));
            return 2;
        }
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

    let (root, path) = match (realpath(&cwd), realpath(&supplied)) {
        (Ok(root), Ok(path)) => (root, path),
        (Err(error), _) | (_, Err(error)) => {
            io.warn(&format!("stanza: {error}"));
            return 2;
        }
    };

    let body = without_mark(&text);
    let display = |path: &str| printed_path(path, Some(&root));
    let request = Request {
        path: &path,
        text: body,
        braces: args.no_braces.then_some(Braces::Off),
        display: &display,
    };

    let result = explain(&request, &[args.line])
        .and_then(|lines| lines.into_iter().next().expect("one line was asked"));

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

    let Some(base) = resolving_cwd(io, [input]) else {
        return 2;
    };

    let target = stdin_target(input, &base, &mut Repositories::default());
    let inputs = match &target {
        StdinTarget::Format { path, .. } => vec![Input {
            path: path.clone(),
            output: printed_path(path, io.cwd.as_deref().ok()),
            text: decode(source.clone()),
            lines: None,
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
        warn_crashed(io, "stanza", &formatted.crashed);
        warn_unread(io, &formatted.unread);
    }

    let errors = match target {
        StdinTarget::Unsupported(error)
        | StdinTarget::Unreadable(error)
        | StdinTarget::Failed(error) => vec![error],
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
        let shown = printed_path(path, io.cwd.as_deref().ok());
        io.warn(&format!(
            "stanza: skipped git submodule {shown}, pass its path to format its files"
        ));
    }
}

fn run_files(args: &Arguments, io: &mut Io) -> i32 {
    let base = if args.changed {
        required_cwd(io)
    } else {
        resolving_cwd(io, args.paths.iter().map(String::as_str))
    };

    let Some(base) = base else {
        return 2;
    };

    let mut repositories = Repositories::default();
    let selected = if args.changed {
        let predicted = repositories.predict(&base);
        prefetch_changed(&[(predicted, None)], &mut repositories);
        let location = repositories.locate(&base);
        collect_changed(location, &mut repositories, None, args.hunks)
            .map(|selected| (selected, None))
    } else {
        collect_files(&args.paths, &base, false, &mut repositories).and_then(|selected| {
            if !selected.files.is_empty() || !selected.errors.is_empty() {
                return Ok((selected, None));
            }

            if selected.opted_out {
                return Ok((selected, Some(EmptyReason::OptedOut)));
            }

            let submodules = selected.submodules.iter().cloned().collect();
            let reason = empty_reason(&args.paths, &base, &submodules, &mut repositories)?;
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

    let cwd = io.cwd.clone().ok();
    let run = Run {
        mode: args.mode,
        braces: args.braces,
        write: true,
    };

    let file_input = |path: &String| Input {
        path: path.clone(),
        output: printed_path(path, cwd.as_deref()),
        text: read_text(path),
        lines: collected.changed_lines.get(path).cloned(),
    };

    let count = worker_count(
        collected.files.len(),
        io.workers.as_deref(),
        DEFAULT_WORKERS,
        cores(),
    );

    let (formatted, pool_warning) = if let Some(stack) = io.stack.filter(|_| count > 0) {
        let pooled = run_pooled(
            &collected.files,
            count,
            stack,
            |path| hard_linked(path, &run),
            |path| format_file(file_input(path), &run),
        );

        (merge_outcomes(pooled.outcomes), pooled.unstarted)
    } else {
        (
            format_inputs(collected.files.iter().map(file_input), &run),
            None,
        )
    };

    let finished = Finished {
        formatted,
        selection_errors: collected.errors,
        pool_warning,
        explains_generated,
    };

    finish_files(args, io, finished)
}

struct Finished {
    formatted: Formatted,
    selection_errors: Vec<String>,
    pool_warning: Option<String>,
    explains_generated: bool,
}

fn finish_files(args: &Arguments, io: &mut Io, finished: Finished) -> i32 {
    let Finished {
        formatted,
        selection_errors,
        pool_warning,
        explains_generated,
    } = finished;

    if let Some(warning) = pool_warning {
        io.warn(&format!("stanza: {warning}"));
    }

    if explains_generated && formatted.only_generated {
        io.warn(&empty_message(EmptyReason::Generated, &args.paths));
    }

    io.out(&printed_findings(&formatted.findings, args.json));
    warn_crashed(io, "stanza", &formatted.crashed);

    for error in &selection_errors {
        io.warn(&format!("stanza: {error}"));
    }

    warn_unread(io, &formatted.unread);
    status(&selection_errors, &formatted)
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

    let (scoped, whole): (Vec<&StagedFile>, Vec<&StagedFile>) = files
        .iter()
        .filter(|file| fixable.contains(printed_path(&file.path, Some(cwd)).as_str()))
        .partition(|file| file.lines.is_some());

    let (dirty, clean): (Vec<&StagedFile>, Vec<&StagedFile>) =
        whole.into_iter().partition(|file| file.unstaged);

    let names = |list: &[&StagedFile]| {
        list.iter()
            .map(|file| shell_word(&printed_path(&file.path, Some(cwd))))
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut lines = Vec::new();
    if !scoped.is_empty() {
        lines.push(format!(
            "fix the staged lines of these by hand and restage them with git add -p, --fix would change the whole file: {}",
            names(&scoped)
        ));
    }

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
    let Some(cwd) = required_cwd(io) else {
        return 2;
    };

    let collected = collect_staged(&cwd, &mut Repositories::default(), args.hunks);
    let files = match &collected {
        Ok(staged) => {
            warn_submodules(io, &staged.submodules);
            staged.files.as_slice()
        }
        Err(_) => &[],
    };

    let inputs = files.iter().map(|file| Input {
        path: file.path.clone(),
        output: printed_path(&file.path, Some(&cwd)),
        text: decode(file.bytes.clone()),
        lines: file.lines.clone(),
    });

    let run = Run {
        mode: args.mode,
        braces: args.braces,
        write: false,
    };

    let formatted = format_inputs(inputs, &run);
    io.out(&printed_findings(&formatted.findings, args.json));
    warn_crashed(io, "stanza", &formatted.crashed);

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

fn emit(io: &mut Io, value: &Json) -> HookExit {
    let mut line = String::new();
    if let Err(error) = stringify(value, &mut line) {
        io.warn(&format!("stanza hook: {error}"));
        return HookExit::Failed;
    }

    line.push('\n');
    io.out(line.as_bytes());
    HookExit::Done
}

fn run_write_hook(
    cwd_input: &str,
    tool_input: &ToolInput,
    flags: &HookFlags,
    io: &mut Io,
) -> HookExit {
    let Ok(cwd) = realpath(cwd_input) else {
        return HookExit::Done;
    };

    let (path, root) = match stdin_target(&tool_input.path(), &cwd, &mut Repositories::default()) {
        StdinTarget::Failed(error) => {
            io.warn(&format!("stanza hook: {error}"));
            return HookExit::Failed;
        }
        StdinTarget::Format {
            path,
            root: Some(root),
        } => (path, root),
        _ => return HookExit::Done,
    };

    if !lands_within(&root, &path) || ignored_by_git(&root, &path) {
        return HookExit::Done;
    }

    if flags.hunks && tracked_in_head(&root, &path) {
        return HookExit::Done;
    }

    let masked = tool_input.masked();
    let input = Input {
        output: printed_path(&path, Some(&cwd)),
        path,
        text: Ok(masked.text.clone()),
        lines: None,
    };

    let run = Run {
        mode: Mode::Fix,
        braces: flags.braces,
        write: false,
    };

    let formatted = format_inputs(std::iter::once(input), &run);
    let updated = formatted
        .kept
        .and_then(|fixed| tool_input.updated(&masked, fixed));

    if let Some(updated) = updated {
        let context = concat(&[
            &JsString::from("stanza formatted "),
            &tool_input.file_path,
            &JsString::from(" before writing it, so read it again before editing."),
        ]);

        let output = object(vec![(
            "hookSpecificOutput",
            object(vec![
                ("hookEventName", text("PreToolUse")),
                ("updatedInput", updated),
                ("additionalContext", Json::String(context)),
            ]),
        )]);

        if emit(io, &output) == HookExit::Failed {
            return HookExit::Failed;
        }
    }

    for line in unread_lines(Some(&cwd), &formatted.unread) {
        io.warn(&line);
    }

    warn_crashed(io, "stanza hook", &formatted.crashed);
    crash_exit(HookExit::Done, &formatted.crashed)
}

fn crash_exit(exit: HookExit, crashed: &[String]) -> HookExit {
    if crashed.is_empty() {
        exit
    } else {
        HookExit::Failed
    }
}

type Group = (Location, Option<IndexSet<String>>);

fn hook_repositories(
    cwd: &str,
    written: Option<&IndexSet<String>>,
    mut locate: impl FnMut(&str) -> Location,
) -> Vec<Group> {
    let Some(written) = written else {
        let location = locate(cwd);
        return match location {
            Location::Outside => Vec::new(),
            location => vec![(location, None)],
        };
    };

    let mut found: IndexMap<String, (Location, IndexSet<String>)> = IndexMap::new();
    for path in written {
        let Ok(real) = realpath(path) else {
            continue;
        };

        let location = locate(&dirname(path));
        let key = match &location {
            Location::Outside => continue,
            Location::Repository(root) if !lands_within(root, &real) => continue,
            Location::Repository(root) => root.clone(),
            Location::Failed(error) => error.clone(),
        };

        found
            .entry(key)
            .or_insert_with(|| (location, IndexSet::new()))
            .1
            .insert(real);
    }

    found
        .into_values()
        .map(|(location, kept)| (location, Some(kept)))
        .collect()
}

fn fix_changed(
    cwd_input: &str,
    written: Option<&IndexSet<String>>,
    flags: &HookFlags,
    io: &mut Io,
) -> Result<Formatted, HookExit> {
    let Ok(cwd) = realpath(cwd_input) else {
        return Err(HookExit::Done);
    };

    let mut repositories = Repositories::default();
    let mut files = Vec::new();
    let mut changed_lines = HashMap::new();
    let mut failed = false;
    let predicted = hook_repositories(&cwd, written, |dir| repositories.predict(dir));
    prefetch_changed(&predicted, &mut repositories);
    for (location, kept) in hook_repositories(&cwd, written, |dir| repositories.locate(dir)) {
        let collected =
            match collect_changed(location, &mut repositories, kept.as_ref(), flags.hunks) {
                Ok(collected) => collected,
                Err(error) => {
                    io.warn(&format!("stanza hook: {error}"));
                    return Err(HookExit::Failed);
                }
            };

        for line in collected.warnings.iter().chain(&collected.errors) {
            io.warn(&format!("stanza hook: {line}"));
        }

        failed |= !collected.errors.is_empty();
        files.extend(collected.files);
        changed_lines.extend(collected.changed_lines);
    }

    if failed {
        return Err(HookExit::Failed);
    }

    let inputs = files.iter().map(|path| Input {
        path: path.clone(),
        output: if lands_within(&cwd, path) {
            printed_path(path, Some(&cwd))
        } else {
            path.clone()
        },
        text: read_text(path),
        lines: changed_lines.remove(path),
    });

    let run = Run {
        mode: Mode::Fix,
        braces: flags.braces,
        write: true,
    };

    let formatted = format_inputs(inputs, &run);
    for line in unread_lines(Some(&cwd), &formatted.unread) {
        io.warn(&line);
    }

    warn_crashed(io, "stanza hook", &formatted.crashed);
    Ok(formatted)
}

fn run_stop_hook(
    cwd: &str,
    written: Option<&IndexSet<String>>,
    flags: &HookFlags,
    io: &mut Io,
) -> HookExit {
    let formatted = match fix_changed(cwd, written, flags, io) {
        Ok(formatted) => formatted,
        Err(exit) => return exit,
    };

    let exit = match block_reason(&formatted.findings, &formatted.rewritten) {
        Some(reason) => {
            let output = object(vec![("decision", text("block")), ("reason", text(&reason))]);
            emit(io, &output)
        }
        None => HookExit::Done,
    };

    crash_exit(exit, &formatted.crashed)
}

fn run_edit_hook(cwd: &str, paths: &[String], flags: &HookFlags, io: &mut Io) -> HookExit {
    let written: IndexSet<String> = paths.iter().cloned().collect();
    let formatted = match fix_changed(cwd, Some(&written), flags, io) {
        Ok(formatted) => formatted,
        Err(exit) => return exit,
    };

    let exit = if formatted.rewritten.is_empty() {
        HookExit::Done
    } else {
        let output = object(vec![(
            "hookSpecificOutput",
            object(vec![
                ("hookEventName", text("PostToolUse")),
                (
                    "additionalContext",
                    text(&reread_line("formatted", &formatted.rewritten)),
                ),
            ]),
        )]);

        emit(io, &output)
    };

    crash_exit(exit, &formatted.crashed)
}

fn run_hook(flags: &HookFlags, io: &mut Io) -> HookExit {
    let bytes = match (io.stdin)() {
        Ok(bytes) => bytes,
        Err(error) => {
            let code = error_code(&error).map_or_else(|| error.to_string(), str::to_owned);
            io.warn(&format!("stanza hook: cannot read stdin: {code}"));
            return HookExit::Failed;
        }
    };

    let fallback_cwd = io.cwd.as_deref().map_err(String::as_str);
    let call = hook_input(&String::from_utf8_lossy(&bytes), fallback_cwd).and_then(hook_call);

    match call {
        Ok(None) => HookExit::Done,
        Ok(Some(HookCall::Write { cwd, tool_input })) => {
            run_write_hook(&cwd, &tool_input, flags, io)
        }
        Ok(Some(HookCall::Edit { cwd, paths })) => run_edit_hook(&cwd, &paths, flags, io),
        Ok(Some(HookCall::Stop { cwd, written })) => {
            run_stop_hook(&cwd, written.as_ref(), flags, io)
        }
        Err(error) => {
            io.warn(&format!("stanza hook: {error}"));
            HookExit::Failed
        }
    }
}

pub enum Command {
    Help,
    Version,
    HookOff,
    Usage {
        prefix: &'static str,
        error: String,
        code: i32,
    },
    Hook(HookFlags),
    Explain(ExplainArguments),
    Format(Arguments),
}

impl Command {
    pub fn parses_source(&self) -> bool {
        matches!(
            self,
            Command::Hook(_) | Command::Explain(_) | Command::Format(_)
        )
    }
}

pub fn command(argv: &[String], agent_hooks: Option<&str>) -> Command {
    let usage = |prefix, code| {
        move |error| Command::Usage {
            prefix,
            error,
            code,
        }
    };

    match argv.first().map(String::as_str) {
        Some("hook") if agent_hooks == Some("0") => Command::HookOff,
        Some("hook") => hook_flags(&argv[1..])
            .map_or_else(usage("stanza hook", HookExit::Failed.code()), Command::Hook),
        Some("explain") => {
            parse_explain(&argv[1..]).map_or_else(usage("stanza", 2), Command::Explain)
        }
        Some("--help" | "-h") if argv.len() == 1 => Command::Help,
        Some("--version") if argv.len() == 1 => Command::Version,
        _ => parse_arguments(argv).map_or_else(usage("stanza", 2), Command::Format),
    }
}

pub fn run(argv: &[String], io: &mut Io) -> i32 {
    let command = command(argv, io.agent_hooks.as_deref());
    execute(&command, io)
}

pub fn execute(command: &Command, io: &mut Io) -> i32 {
    let code = catch_panic(|| dispatch(command, io)).unwrap_or_else(|panic| {
        let (prefix, code) = match command {
            Command::Hook(_) => ("stanza hook", HookExit::Failed.code()),
            _ => ("stanza", 2),
        };

        io.warn(&format!("{prefix}: internal error: {panic}"));
        code
    });

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
        Command::Usage {
            prefix,
            error,
            code,
        } => {
            io.warn(&format!("{prefix}: {error}\n{USAGE}"));
            *code
        }
        Command::Hook(flags) => run_hook(flags, io).code(),
        Command::Explain(args) => run_explain(args, io),
        Command::Format(args) => match &args.stdin {
            Some(input) => run_stdin(input, args, io),
            None if args.staged => run_staged(args, io),
            None => run_files(args, io),
        },
    }
}

fn required_cwd(io: &mut Io) -> Option<String> {
    match io.cwd.clone() {
        Ok(cwd) => Some(cwd),
        Err(error) => {
            io.warn(&format!("stanza: {error}"));
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
    use super::*;
    use crate::pool::STACK_LADDER;

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

        let args = parse_arguments(&["--check".to_owned(), ".".to_owned()]).unwrap();
        let finished = Finished {
            formatted: Formatted {
                findings: Vec::new(),
                failed: false,
                only_generated: true,
                kept: None,
                rewritten: Vec::new(),
                unread: Unread::default(),
                crashed: Vec::new(),
            },
            selection_errors: Vec::new(),
            pool_warning: pool_warning.map(str::to_owned),
            explains_generated: true,
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
            cwd: Ok("/".to_owned()),
            stack: Some(STACK_LADDER[0]),
            workers: None,
            agent_hooks: None,
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

    #[test]
    fn pre_tool_use_echoes_tool_input_as_the_reference_recorded() {
        let table: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../tests/fixtures/hook/updated-input.json"
        ))
        .unwrap();

        assert!(table.len() >= 16);
        for (index, row) in table.iter().enumerate() {
            let name = row["name"].as_str().unwrap();
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

            let expected = row["stdout"].as_str().unwrap().to_owned();
            let observed = hook_in(&dir, hex(row["stdin"].as_str().unwrap()));
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!(observed, (expected, String::new(), 0), "{name}");
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

    const UNREADABLE: &str = "cannot read the working directory: ENOENT";

    #[test]
    fn only_commands_that_parse_source_ask_for_a_stack() {
        let parses = |argv: &[&str], agent_hooks: Option<&str>| {
            let argv: Vec<String> = argv.iter().map(|&arg| arg.to_owned()).collect();
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
