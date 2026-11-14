use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};
use serde::Serialize;
use serde_json::value::RawValue;

use super::args::HookFlags;
use super::report::{invalid_lines, printed_path, unread_lines, warn_crashed};
use super::{Io, Program};
use crate::engine::types::{Located, Mode};
use crate::files::{
    Location, Repositories, StdinTarget, Submodules, collect_changed, error_code, ignored_by_git,
    lands_within, prefetch_changed, realpath, stdin_target, tracked_in_head,
};
use crate::format::{Formatted, Input, Run, format_inputs, read_text};
use crate::hook::{
    HookCall, HookExit, ToolInput, block_reason, hook_call, hook_input, reread_line,
};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_input: Option<&'a RawValue>,
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

fn run_write_hook(
    cwd_input: &str,
    tool_input: &ToolInput,
    flags: &HookFlags,
    io: &mut Io,
) -> HookExit {
    let Ok(cwd) = realpath(cwd_input) else {
        return HookExit::Done;
    };

    let (path, root) = match stdin_target(&tool_input.path, &cwd, &mut Repositories::default()) {
        StdinTarget::Failed(error) => {
            io.error(Program::Hook, error);
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

    let input = Input {
        output: printed_path(&path, Some(&cwd)),
        path,
        text: Ok(tool_input.content.clone()),
        lines: None,
    };

    let run = Run {
        mode: Mode::Fix,
        braces: flags.braces,
        write: false,
    };

    let formatted = format_inputs(std::iter::once(input), &run);
    if let Some(fixed) = &formatted.kept {
        let echo = match tool_input.echo(fixed) {
            Ok(echo) => echo,
            Err(error) => {
                io.error(Program::Hook, format_args!("internal error ({error})"));
                return HookExit::Failed;
            }
        };

        let context = format!(
            "stanza formatted {} before writing it, so read it again before editing.",
            tool_input.path
        );

        let output = SpecificOutput {
            hook_specific_output: Specific {
                hook_event_name: "PreToolUse",
                updated_input: Some(&echo),
                additional_context: &context,
            },
        };

        if emit(io, &output) == HookExit::Failed {
            return HookExit::Failed;
        }
    }

    for line in unread_lines(Some(&cwd), &formatted.unread) {
        io.error(Program::Stanza, line);
    }

    warn_crashed(io, Program::Hook, &formatted.crashed);
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
) -> Result<(Formatted, bool), HookExit> {
    let Ok(cwd) = realpath(cwd_input) else {
        return Err(HookExit::Done);
    };

    let mut repositories = Repositories::default();
    let mut files = Vec::new();
    let mut changed_lines = HashMap::new();
    let mut failed = false;
    let mut invalid = false;
    let predicted = hook_repositories(&cwd, written, |dir| repositories.predict(dir));
    prefetch_changed(&predicted, Submodules::Unnamed, &mut repositories);
    for (location, kept) in hook_repositories(&cwd, written, |dir| repositories.locate(dir)) {
        let collected = match collect_changed(
            location,
            &mut repositories,
            kept.as_ref(),
            Submodules::Unnamed,
            flags.hunks,
        ) {
            Ok(collected) => collected,
            Err(error) => {
                io.error(Program::Hook, error);
                return Err(HookExit::Failed);
            }
        };

        for line in collected.warnings.iter().chain(&collected.errors) {
            io.error(Program::Hook, line);
        }

        for line in invalid_lines(Some(&cwd), &collected.invalid_names) {
            io.error(Program::Hook, line);
        }

        failed |= !collected.errors.is_empty();
        invalid |= !collected.invalid_names.is_empty();
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
        io.error(Program::Stanza, line);
    }

    warn_crashed(io, Program::Hook, &formatted.crashed);
    Ok((formatted, invalid))
}

fn run_stop_hook(
    cwd: &str,
    written: Option<&IndexSet<String>>,
    flags: &HookFlags,
    io: &mut Io,
) -> HookExit {
    let (formatted, invalid) = match fix_changed(cwd, written, flags, io) {
        Ok(fixed) => fixed,
        Err(exit) => return exit,
    };

    let located: Vec<Located> = formatted.findings.ordered().collect();
    let exit = match block_reason(&located, &formatted.rewritten) {
        Some(reason) => emit(
            io,
            &Block {
                decision: "block",
                reason: &reason,
            },
        ),
        None => HookExit::Done,
    };

    if invalid {
        HookExit::Failed
    } else {
        crash_exit(exit, &formatted.crashed)
    }
}

fn run_edit_hook(cwd: &str, paths: &[String], flags: &HookFlags, io: &mut Io) -> HookExit {
    let written: IndexSet<String> = paths.iter().cloned().collect();
    let (formatted, invalid) = match fix_changed(cwd, Some(&written), flags, io) {
        Ok(fixed) => fixed,
        Err(exit) => return exit,
    };

    let exit = if formatted.rewritten.is_empty() {
        HookExit::Done
    } else {
        let context = reread_line("formatted", &formatted.rewritten);
        let output = SpecificOutput {
            hook_specific_output: Specific {
                hook_event_name: "PostToolUse",
                updated_input: None,
                additional_context: &context,
            },
        };

        emit(io, &output)
    };

    if invalid {
        HookExit::Failed
    } else {
        crash_exit(exit, &formatted.crashed)
    }
}

pub(crate) fn run_hook(flags: &HookFlags, io: &mut Io) -> HookExit {
    let bytes = match (io.stdin)() {
        Ok(bytes) => bytes,
        Err(error) => {
            let code = error_code(&error).map_or_else(|| error.to_string(), str::to_owned);
            io.error(Program::Hook, format_args!("cannot read stdin: {code}"));
            return HookExit::Failed;
        }
    };

    let fallback_cwd = io.cwd.as_deref().map_err(String::as_str);
    let call = hook_input(&String::from_utf8_lossy(&bytes), fallback_cwd).map(hook_call);

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
            io.error(Program::Hook, error);
            HookExit::Failed
        }
    }
}
