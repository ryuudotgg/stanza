use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;

use indexmap::IndexSet;

use super::args::Arguments;
use super::io::Lost;
use super::report::{
    Report, Scope, empty_lines, empty_message, invalid_lines, printed_path, repair_lines, status,
    summary_line, warn_crashed, warn_submodules, warn_unread, write_findings,
};
use super::{Io, Program, required_cwd, resolving_cwd};
use crate::engine::types::Mode;
use crate::files::{
    Caller, EmptyReason, PathSelection, Repositories, Seen, StdinTarget, collect_changed,
    collect_files, collect_staged, empty_reasons, invalid_arguments, prefetch_changed, reason,
    stdin_target,
};
use crate::format::{
    Formatted, Input, Run, format_file, format_inputs, hard_linked, merge_outcomes, read_text,
};
use crate::paths::quoted;
use crate::pool::{cores, run_pooled, worker_count};
use crate::step::decode;

pub(crate) fn run_stdin(input: &str, args: &Arguments, io: &mut Io) -> i32 {
    let source = match (io.stdin)() {
        Ok(source) => source,
        Err(error) => {
            io.error(
                Program::Stanza,
                format_args!("cannot read stdin: {}", reason(&error)),
            );
            return 2;
        }
    };

    let Some(base) = resolving_cwd(io, [input]) else {
        return 2;
    };

    let target = stdin_target(input, &base, &mut Repositories::new(io.cwd.as_deref().ok()));
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
    let report = Report::piped(args.json);
    if args.mode == Mode::Fix {
        io.out(
            formatted
                .kept
                .as_ref()
                .map_or(&source[..], String::as_bytes),
        );
        let written = write_findings(&mut io.stderr, &formatted.findings, report);
        io.record(written, Lost::Results);
    } else {
        io.out_findings(&formatted.findings, report);
    }

    if !(args.mode == Mode::Fix && args.json) {
        warn_crashed(io, Program::Stanza, &formatted.crashed);
        warn_unread(io, &formatted.unread);
    }

    let errors = match target {
        StdinTarget::Unsupported(error)
        | StdinTarget::Unreadable(error)
        | StdinTarget::Failed(error) => vec![error],
        StdinTarget::Format { .. } | StdinTarget::Skip(_) => Vec::new(),
    };

    for error in &errors {
        io.error(Program::Stanza, error);
    }

    status(&errors, &formatted)
}

pub(crate) fn run_paths(
    paths: &[String],
    invalid: &[OsString],
    args: &Arguments,
    io: &mut Io,
) -> i32 {
    if paths.iter().any(String::is_empty) {
        io.error(
            Program::Stanza,
            "\"\" is not a path, pass . for the current folder",
        );
        return 2;
    }

    let Some(base) = resolving_cwd(io, paths.iter().map(String::as_str)) else {
        return 2;
    };

    let mut repositories = Repositories::new(io.cwd.as_deref().ok());
    let mut selected = collect_files(paths, &base, false, &mut repositories);
    let arguments = invalid_arguments(invalid, &base, &mut repositories);
    selected.invalid_names.extend(arguments.names);
    selected.invalid_names.sort();
    selected.invalid_names.dedup();
    selected.opted_out |= arguments.arguments.iter().any(|seen| seen.opted_out);

    let invalid = invalid.iter().map(|path| quoted(path.as_bytes()));
    let named: Vec<String> = paths.iter().cloned().chain(invalid).collect();
    let explanation = Explanation {
        paths: paths.to_vec(),
        base,
        boundaries: PathSelection {
            arguments: std::mem::take(&mut selected.arguments),
            submodules: selected.submodules.clone(),
            nested: std::mem::take(&mut selected.nested),
            ..PathSelection::default()
        },
        invalid: arguments.arguments,
        named,
        cwd: io.cwd.clone().ok(),
        repositories,
    };

    let selected_nothing = selected.files.is_empty()
        && selected.errors.is_empty()
        && selected.invalid_names.is_empty();

    if !selected_nothing {
        let generated: Box<dyn FnOnce() -> Vec<String>> = Box::new(|| explanation.generated());
        return format_selected(Ok((selected, Vec::new())), Some(generated), args, io);
    }

    let empty = match explanation.lines(false) {
        Ok(lines) => lines,
        Err(failure) => {
            selected.errors.push(failure);
            Vec::new()
        }
    };

    format_selected(Ok((selected, empty)), None, args, io)
}

struct Explanation {
    paths: Vec<String>,
    base: String,
    boundaries: PathSelection,
    invalid: Vec<Seen>,
    named: Vec<String>,
    cwd: Option<String>,
    repositories: Repositories,
}

impl Explanation {
    fn lines(mut self, header_generated: bool) -> Result<Vec<String>, String> {
        let reasons = empty_reasons(
            &self.paths,
            &self.base,
            &self.boundaries,
            &mut self.repositories,
        )?;

        let invalid = self.invalid.iter().map(|seen| {
            Some(if seen.opted_out {
                EmptyReason::OptedOut
            } else if seen.generated {
                EmptyReason::Generated
            } else {
                EmptyReason::None
            })
        });

        let seen = self.boundaries.arguments.iter().chain(&self.invalid);
        let reasons: Vec<_> = reasons
            .into_iter()
            .chain(invalid)
            .zip(seen)
            .map(|(reason, seen)| match reason {
                _ if header_generated && seen.kept => Some(EmptyReason::Generated),
                reason => reason,
            })
            .collect();

        let nested = &self.boundaries.nested;
        Ok(empty_lines(
            &self.named,
            &reasons,
            nested,
            self.cwd.as_deref(),
        ))
    }

    fn generated(self) -> Vec<String> {
        let fallback = empty_message(EmptyReason::Generated, &self.named);
        self.lines(true).unwrap_or_else(|_| vec![fallback])
    }
}

pub(crate) fn run_changed(hunks: bool, args: &Arguments, io: &mut Io) -> i32 {
    let Some(base) = required_cwd(io) else {
        return 2;
    };

    let mut repositories = Repositories::new(Some(&base));
    let predicted = repositories.predict(&base);
    prefetch_changed(
        &[(predicted, None)],
        Caller::Cli(&base),
        hunks,
        &mut repositories,
    );
    let location = repositories.locate(&base);
    let selected = collect_changed(location, &mut repositories, None, Caller::Cli(&base), hunks)
        .map(|selected| (selected, Vec::new()));

    format_selected(selected, None, args, io)
}

fn format_selected(
    selected: Result<(PathSelection, Vec<String>), String>,
    generated: Option<Box<dyn FnOnce() -> Vec<String>>>,
    args: &Arguments,
    io: &mut Io,
) -> i32 {
    let (mut collected, empty) = match selected {
        Ok(selected) => selected,
        Err(error) => {
            io.error(
                Program::Stanza,
                format_args!("file selection failed: {error}"),
            );
            return 2;
        }
    };

    let invalid = invalid_lines(io.cwd.as_deref().ok(), &collected.invalid_names);
    let errors: IndexSet<String> = collected.errors.drain(..).chain(invalid).collect();
    collected.errors = errors.into_iter().collect();

    let generated = generated.filter(|_| !collected.opted_out && collected.errors.is_empty());

    for warning in &collected.warnings {
        io.notice(Program::Stanza, warning);
    }

    warn_submodules(io, &collected.submodules);
    for line in empty {
        io.notice(Program::Stanza, line);
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

    let count = worker_count(collected.files.len(), io.workers.as_deref(), cores());

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
        generated,
    };

    finish_files(args, io, finished)
}

pub(crate) struct Finished {
    pub(crate) formatted: Formatted,
    pub(crate) selection_errors: Vec<String>,
    pub(crate) pool_warning: Option<String>,
    pub(crate) generated: Option<Box<dyn FnOnce() -> Vec<String>>>,
}

pub(crate) fn finish_files(args: &Arguments, io: &mut Io, finished: Finished) -> i32 {
    let Finished {
        formatted,
        selection_errors,
        pool_warning,
        generated,
    } = finished;

    if let Some(warning) = pool_warning {
        io.notice(Program::Stanza, warning);
    }

    if let Some(generated) = generated.filter(|_| formatted.only_generated()) {
        for line in generated() {
            io.notice(Program::Stanza, line);
        }
    }

    io.out_findings(&formatted.findings, Report::styled(args.json, io.look));
    warn_crashed(io, Program::Stanza, &formatted.crashed);

    for error in &selection_errors {
        io.error(Program::Stanza, error);
    }

    warn_unread(io, &formatted.unread);
    summarize(args, io, &formatted, &selection_errors);
    status(&selection_errors, &formatted)
}

fn summarize(args: &Arguments, io: &mut Io, formatted: &Formatted, errors: &[String]) {
    let Some(scope) = Scope::of(&args.source).filter(|_| !args.json) else {
        return;
    };

    let flushed = io.stdout.flush();
    io.record(flushed, Lost::Results);
    if io
        .unwritten
        .as_ref()
        .is_some_and(|unwritten| unwritten.lost == Lost::Results)
    {
        return;
    }

    if let Some(line) = summary_line(args.mode, scope, formatted, errors) {
        io.summary(&line);
    }
}

pub(crate) fn run_staged(hunks: bool, args: &Arguments, io: &mut Io) -> i32 {
    let Some(cwd) = required_cwd(io) else {
        return 2;
    };

    let collected = collect_staged(&cwd, &mut Repositories::new(Some(&cwd)), hunks);
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
    io.out_findings(&formatted.findings, Report::styled(args.json, io.look));
    warn_crashed(io, Program::Stanza, &formatted.crashed);

    let errors = match &collected {
        Ok(staged) => invalid_lines(Some(&cwd), &staged.invalid_names),
        Err(error) => vec![error.clone()],
    };

    for error in &errors {
        io.error(Program::Stanza, error);
    }

    warn_unread(io, &formatted.unread);
    if collected.is_ok() {
        summarize(args, io, &formatted, &errors);
        for line in repair_lines(&formatted.findings, files, &cwd, args.braces) {
            io.warn(&line);
        }
    }

    status(&errors, &formatted)
}
