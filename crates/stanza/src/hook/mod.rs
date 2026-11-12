mod block;
mod input;
mod rollout;
mod transcript;

use indexmap::IndexSet;

use crate::files::realpath;
use crate::languages::is_candidate;
use crate::paths::basename;
use input::{HookInput, Transcript};
use transcript::{recorded_writes, written_files};

pub use block::{block_reason, reread_line};
pub use input::{ToolInput, hook_input};

pub const CLAUDE_CODE_HOOKS: &str = r#"{ "hooks": { "PreToolUse": [{ "matcher": "Write", "hooks": [{ "type": "command", "command": "stanza hook" }] }], "PostToolUse": [{ "matcher": "Edit|MultiEdit", "hooks": [{ "type": "command", "command": "stanza hook" }] }], "Stop": [{ "hooks": [{ "type": "command", "command": "stanza hook" }] }] } }"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookExit {
    Done,
    Failed,
}

impl HookExit {
    pub fn code(self) -> i32 {
        match self {
            HookExit::Done => 0,
            HookExit::Failed => 1,
        }
    }
}

#[derive(Debug)]
pub enum HookCall {
    Write {
        cwd: String,
        tool_input: ToolInput,
    },
    Edit {
        cwd: String,
        paths: Vec<String>,
    },
    Stop {
        cwd: String,
        written: Option<IndexSet<String>>,
    },
}

fn is_candidate_file(path: &str) -> bool {
    realpath(path).is_ok_and(|real| is_candidate(basename(&real), false))
}

pub fn hook_call(input: HookInput) -> Option<HookCall> {
    let (cwd, stop_hook_active, transcript) = match input {
        HookInput::Ignored => return None,
        HookInput::Write { cwd, tool_input } => {
            let candidate = is_candidate(basename(&tool_input.path()), false);
            return candidate.then_some(HookCall::Write { cwd, tool_input });
        }
        HookInput::Edit { cwd, paths } => {
            let candidate = paths.iter().any(|path| is_candidate_file(path));
            return candidate.then_some(HookCall::Edit { cwd, paths });
        }
        HookInput::Stop {
            cwd,
            stop_hook_active,
            transcript,
        } => (cwd, stop_hook_active, transcript),
    };

    if stop_hook_active {
        return None;
    }

    let written = match transcript {
        Transcript::Agent(None) => return None,
        Transcript::Agent(Some(path)) => match recorded_writes(&path, true) {
            Some((_, written)) if written.iter().any(|path| is_candidate_file(path)) => written,
            _ => return None,
        },
        Transcript::Session(path) => {
            let written = path.and_then(|path| written_files(&path));
            if written
                .as_ref()
                .is_some_and(|written| !written.iter().any(|path| is_candidate_file(path)))
            {
                return None;
            }

            return Some(HookCall::Stop { cwd, written });
        }
    };

    Some(HookCall::Stop {
        cwd,
        written: Some(written),
    })
}
