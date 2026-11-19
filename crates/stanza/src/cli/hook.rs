use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};
use serde::Serialize;

use super::args::HookFlags;
use super::report::{invalid_lines, printed_path, unread_lines, warn_crashed};
use super::{Io, Program};
use crate::engine::types::Mode;
use crate::files::{
    Caller, Changed, Location, Repositories, collect_changed, lands_within, prefetch_changed,
    realpath, reason,
};
use crate::format::{Formatted, Input, Run, format_inputs, read_text};
use crate::hook::{HookCall, HookExit, block_reason, hook_call, hook_input, reread_line};
use crate::paths::dirname;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpecificOutput<'a> {
    hook_specific_output: Specific<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Specific<'a> {
    hook_event_name: &'static str,
    additional_context: &'a str,
}

#[derive(Serialize)]
struct Block<'a> {
    decision: &'static str,
    reason: &'a str,
}

fn emit(io: &mut Io, value: &impl Serialize) -> HookExit {
    let mut line = Vec::new();
    if let Err(error) = serde_json::to_writer(&mut line, value) {
        io.error(Program::Hook, error);
        return HookExit::Failed;
    }

    line.push(b'\n');
    io.out(&line);
    HookExit::Done
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
    cwd: Option<&str>,
    written: Option<&IndexSet<String>>,
    mut locate: impl FnMut(&str) -> Location,
) -> Vec<Group> {
    let Some(written) = written else {
        return match cwd.map(locate) {
            None | Some(Location::Outside) => Vec::new(),
            Some(location) => vec![(location, None)],
        };
    };

    let mut found: IndexMap<String, (Location, IndexSet<String>)> = IndexMap::new();
    for path in written {
        let Ok(real) = realpath(path) else {
            continue;
        };

        let location = locate(dirname(path));
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
) -> (Formatted, bool) {
    let cwd = realpath(cwd_input).ok();
    let cwd = cwd.as_deref();

    let (files, mut changed_lines, failed) = select_changed(cwd, written, flags.hunks, io);
    let inputs = files.iter().map(|path| Input {
        path: path.clone(),
        output: match cwd {
            Some(cwd) if lands_within(cwd, path) => printed_path(path, Some(cwd)),
            _ => path.clone(),
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
    for line in unread_lines(cwd, &formatted.unread) {
        io.notice(Program::Hook, line);
    }

    warn_crashed(io, Program::Hook, &formatted.crashed);
    (formatted, failed)
}

fn select_changed(
    cwd: Option<&str>,
    written: Option<&IndexSet<String>>,
    hunks: bool,
    io: &mut Io,
) -> (Vec<String>, HashMap<String, Changed>, bool) {
    let mut repositories = Repositories::new(cwd);
    let mut files = Vec::new();
    let mut changed_lines = HashMap::new();
    let mut failed = false;
    let predicted = hook_repositories(cwd, written, |dir| repositories.predict(dir));
    prefetch_changed(&predicted, Caller::Hook, hunks, &mut repositories);
    for (location, kept) in hook_repositories(cwd, written, |dir| repositories.locate(dir)) {
        let collected = match collect_changed(
            location,
            &mut repositories,
            kept.as_ref(),
            Caller::Hook,
            hunks,
        ) {
            Ok(collected) => collected,
            Err(error) => {
                io.error(Program::Hook, error);
                failed = true;
                continue;
            }
        };

        for line in &collected.warnings {
            io.notice(Program::Hook, line);
        }

        for line in &collected.errors {
            io.error(Program::Hook, line);
        }

        for line in invalid_lines(cwd, &collected.invalid_names) {
            io.error(Program::Hook, line);
        }

        failed |= !collected.errors.is_empty() || !collected.invalid_names.is_empty();
        files.extend(collected.files);
        changed_lines.extend(collected.changed_lines);
    }

    (files, changed_lines, failed)
}

fn run_stop_hook(
    cwd: &str,
    written: Option<&IndexSet<String>>,
    flags: &HookFlags,
    io: &mut Io,
) -> HookExit {
    let (formatted, failed) = fix_changed(cwd, written, flags, io);

    let exit = match block_reason(&formatted.findings, &formatted.rewritten) {
        Some(reason) => emit(
            io,
            &Block {
                decision: "block",
                reason: &reason,
            },
        ),
        None => HookExit::Done,
    };

    if failed {
        HookExit::Failed
    } else {
        crash_exit(exit, &formatted.crashed)
    }
}

fn run_edit_hook(cwd: &str, paths: &[String], flags: &HookFlags, io: &mut Io) -> HookExit {
    let written: IndexSet<String> = paths.iter().cloned().collect();
    let (formatted, failed) = fix_changed(cwd, Some(&written), flags, io);

    let exit = if formatted.rewritten.is_empty() {
        HookExit::Done
    } else {
        let context = reread_line("formatted", &formatted.rewritten);
        let output = SpecificOutput {
            hook_specific_output: Specific {
                hook_event_name: "PostToolUse",
                additional_context: &context,
            },
        };

        emit(io, &output)
    };

    if failed {
        HookExit::Failed
    } else {
        crash_exit(exit, &formatted.crashed)
    }
}

pub(crate) fn run_hook(flags: &HookFlags, io: &mut Io) -> HookExit {
    let bytes = match (io.stdin)() {
        Ok(bytes) => bytes,
        Err(error) => {
            io.error(
                Program::Hook,
                format_args!("cannot read stdin: {}", reason(&error)),
            );
            return HookExit::Failed;
        }
    };

    let Ok(text) = std::str::from_utf8(&bytes) else {
        io.error(Program::Hook, "input must be valid UTF-8");
        return HookExit::Failed;
    };

    let fallback_cwd = io.cwd.as_deref().map_err(String::as_str);
    let call = hook_input(text, fallback_cwd).map(hook_call);

    match call {
        Ok(None) => HookExit::Done,
        Ok(Some(HookCall::Edit { cwd, paths })) => run_edit_hook(&cwd, &paths, flags, io),
        Ok(Some(HookCall::Stop { cwd, written })) => {
            run_stop_hook(&cwd, written.as_ref(), flags, io)
        }
        Err(error) => {
            io.error(Program::Hook, error);
            HookExit::Failed
        }
    }
}
