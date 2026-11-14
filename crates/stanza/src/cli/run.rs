use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

use indexmap::IndexSet;

use super::args::Arguments;
use super::io::Lost;
use super::report::{
    empty_message, invalid_lines, printed_path, repair_lines, status, warn_crashed,
    warn_submodules, warn_unread, write_findings,
};
use super::{Io, Program, required_cwd, resolving_cwd};
use crate::engine::types::Mode;
use crate::files::{
    Caller, EmptyReason, PathSelection, Repositories, StdinTarget, collect_changed, collect_files,
    collect_staged, empty_reason, invalid_arguments, prefetch_changed, reason, stdin_target,
};
use crate::format::{
    Formatted, Input, Run, format_file, format_inputs, hard_linked, merge_outcomes, read_text,
};
use crate::paths::quoted;
use crate::pool::{DEFAULT_WORKERS, cores, run_pooled, worker_count};
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
    if args.mode == Mode::Fix {
        io.out(
            formatted
                .kept
                .as_ref()
                .map_or(&source[..], String::as_bytes),
        );
        let written = write_findings(&mut io.stderr, &formatted.findings, args.json);
        io.record(written, Lost::Results);
    } else {
        io.out_findings(&formatted.findings, args.json);
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
    let Some(base) = resolving_cwd(io, paths.iter().map(String::as_str)) else {
        return 2;
    };

    let mut repositories = Repositories::default();
    let selected =
        collect_files(paths, &base, false, &mut repositories).and_then(|mut selected| {
            let invalid = invalid_arguments(invalid, &base, &mut repositories);
            selected.invalid_names.extend(invalid.names);
            selected.invalid_names.sort();
            selected.invalid_names.dedup();
            selected.opted_out |= invalid.opted_out;
            if !selected.files.is_empty()
                || !selected.errors.is_empty()
                || !selected.invalid_names.is_empty()
            {
                return Ok((selected, None));
            }

            if selected.opted_out {
                return Ok((selected, Some(EmptyReason::OptedOut)));
            }

            let submodules = selected.submodules.iter().cloned().collect();
            let reason = empty_reason(paths, &base, &submodules, &mut repositories)?;
            let reason =
                (reason != EmptyReason::None || selected.submodules.is_empty()).then_some(reason);
            Ok((selected, reason))
        });

    let invalid = invalid.iter().map(|path| quoted(path.as_bytes()));
    let named: Vec<String> = paths.iter().cloned().chain(invalid).collect();
    let selected = selected.map(|(selected, reason)| {
        let empty = reason.map(|reason| empty_message(reason, &named));
        (selected, empty)
    });

    let generated = empty_message(EmptyReason::Generated, &named);
    format_selected(selected, Some(generated), args, io)
}

pub(crate) fn run_changed(hunks: bool, args: &Arguments, io: &mut Io) -> i32 {
    let Some(base) = required_cwd(io) else {
        return 2;
    };

    let mut repositories = Repositories::default();
    let predicted = repositories.predict(&base);
    prefetch_changed(&[(predicted, None)], Caller::Cli(&base), &mut repositories);
    let location = repositories.locate(&base);
    let selected = collect_changed(location, &mut repositories, None, Caller::Cli(&base), hunks)
        .map(|selected| (selected, None));

    format_selected(selected, None, args, io)
}

fn format_selected(
    selected: Result<(PathSelection, Option<String>), String>,
    generated: Option<String>,
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
        io.error(Program::Stanza, warning);
    }

    warn_submodules(io, &collected.submodules);
    if let Some(empty) = empty {
        io.error(Program::Stanza, empty);
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
        generated,
    };

    finish_files(args, io, finished)
}

pub(crate) struct Finished {
    pub(crate) formatted: Formatted,
    pub(crate) selection_errors: Vec<String>,
    pub(crate) pool_warning: Option<String>,
    pub(crate) generated: Option<String>,
}

pub(crate) fn finish_files(args: &Arguments, io: &mut Io, finished: Finished) -> i32 {
    let Finished {
        formatted,
        selection_errors,
        pool_warning,
        generated,
    } = finished;

    if let Some(warning) = pool_warning {
        io.error(Program::Stanza, warning);
    }

    if let Some(generated) = generated.filter(|_| formatted.only_generated) {
        io.error(Program::Stanza, generated);
    }

    io.out_findings(&formatted.findings, args.json);
    warn_crashed(io, Program::Stanza, &formatted.crashed);

    for error in &selection_errors {
        io.error(Program::Stanza, error);
    }

    warn_unread(io, &formatted.unread);
    status(&selection_errors, &formatted)
}

pub(crate) fn run_staged(hunks: bool, args: &Arguments, io: &mut Io) -> i32 {
    let Some(cwd) = required_cwd(io) else {
        return 2;
    };

    let collected = collect_staged(&cwd, &mut Repositories::default(), hunks);
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
    io.out_findings(&formatted.findings, args.json);
    warn_crashed(io, Program::Stanza, &formatted.crashed);

    let errors = match &collected {
        Ok(staged) => {
            for line in repair_lines(&formatted.findings, files, &cwd, args.braces) {
                io.warn(&line);
            }

            let invalid = invalid_lines(Some(&cwd), &staged.invalid_names);
            for line in &invalid {
                io.error(Program::Stanza, line);
            }

            invalid
        }
        Err(error) => {
            io.error(Program::Stanza, error);
            vec![error.clone()]
        }
    };

    warn_unread(io, &formatted.unread);
    status(&errors, &formatted)
}
