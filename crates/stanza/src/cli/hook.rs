use std::collections::HashMap;

use indexmap::IndexSet;
use serde::Serialize;

use super::args::HookFlags;
use super::report::{invalid_lines, printed_path, warn_crashed, warn_unread};
use super::run::format_files;
use super::{Io, Program};
use crate::engine::types::Mode;
use crate::files::{
    Caller, Changed, Location, Repositories, Targets, collect_changed, lands_within, locate_changed,
};
use crate::format::{Formatted, Run};
use crate::hook::{HookCall, block_reason, hook_call, hook_input, reread_line};
use crate::os::{realpath, reason};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HookExit {
    Done,
    Failed,
}

impl HookExit {
    pub(crate) fn code(self) -> i32 {
        match self {
            HookExit::Done => 0,
            HookExit::Failed => 1,
        }
    }
}

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

fn emit(io: &mut Io, response: &impl Serialize) {
    let mut line = serde_json::to_vec(response).expect("a response struct always serializes");
    line.push(b'\n');
    io.out(&line);
}

fn fix_changed(
    cwd_input: &str,
    written: Option<&IndexSet<String>>,
    flags: &HookFlags,
    io: &mut Io,
) -> (Formatted, HookExit) {
    let cwd = realpath(cwd_input).ok();
    let cwd = cwd.as_deref();

    let (files, changed_lines, failed) = select_changed(cwd, written, flags.hunks, io);
    let printed = |path: &str| match cwd {
        Some(cwd) if lands_within(cwd, path) => printed_path(path, Some(cwd)),
        _ => path.to_owned(),
    };

    let run = Run {
        mode: Mode::Fix,
        braces: flags.braces,
        write: true,
    };

    let (formatted, pool_warning) = format_files(&files, &changed_lines, printed, &run, io);
    if let Some(warning) = pool_warning {
        io.notice(Program::Hook, warning);
    }

    warn_unread(io, Program::Hook, cwd, &formatted.unread);
    warn_crashed(io, Program::Hook, &formatted.crashed);

    let exit = if failed || !formatted.crashed.is_empty() {
        HookExit::Failed
    } else {
        HookExit::Done
    };

    (formatted, exit)
}

fn select_changed(
    cwd: Option<&str>,
    written: Option<&IndexSet<String>>,
    hunks: bool,
    io: &mut Io,
) -> (Vec<String>, HashMap<String, Changed>, bool) {
    let targets = match (written, cwd) {
        (Some(written), _) => Targets::Written(written),
        (None, Some(cwd)) => Targets::Directory(cwd),
        (None, None) => return (Vec::new(), HashMap::new(), false),
    };

    let mut repositories = Repositories::new(cwd);
    let mut files = Vec::new();
    let mut changed_lines = HashMap::new();
    let mut failed = false;
    let located = locate_changed(targets, Caller::Hook, hunks, &mut repositories);
    for (location, kept) in located
        .into_iter()
        .filter(|(location, _)| *location != Location::Outside)
    {
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

        for line in &collected.errors {
            io.error(Program::Hook, line);
        }

        for line in invalid_lines(cwd, &collected.non_utf8_names) {
            io.error(Program::Hook, line);
        }

        failed |= !collected.errors.is_empty() || !collected.non_utf8_names.is_empty();
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
    let (formatted, exit) = fix_changed(cwd, written, flags, io);
    if let Some(reason) = block_reason(&formatted.findings, &formatted.rewritten) {
        let block = Block {
            decision: "block",
            reason: &reason,
        };

        emit(io, &block);
    }

    exit
}

fn run_edit_hook(cwd: &str, paths: &[String], flags: &HookFlags, io: &mut Io) -> HookExit {
    let written: IndexSet<String> = paths.iter().cloned().collect();
    let (formatted, exit) = fix_changed(cwd, Some(&written), flags, io);
    if !formatted.rewritten.is_empty() {
        let context = reread_line("formatted", &formatted.rewritten);
        let output = SpecificOutput {
            hook_specific_output: Specific {
                hook_event_name: "PostToolUse",
                additional_context: &context,
            },
        };

        emit(io, &output);
    }

    exit
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
