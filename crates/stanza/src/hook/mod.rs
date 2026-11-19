mod block;
mod input;
mod rollout;
mod transcript;

use indexmap::IndexSet;

use crate::files::realpath;
use crate::languages::is_candidate;
use crate::paths::{basename, is_absolute, resolve};
use input::Transcript;
use transcript::{recorded_writes, written_files};

pub use block::{block_reason, reread_line};
pub use input::{HookInput, ToolInput, hook_input};

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

fn names_a_file(cwd: &str, path: &str) -> bool {
    is_absolute(path) || realpath(cwd).is_ok()
}

pub fn hook_call(input: HookInput) -> Option<HookCall> {
    let (cwd, stop_hook_active, transcript) = match input {
        HookInput::Ignored => return None,
        HookInput::Write { cwd, tool_input } => {
            let candidate = is_candidate(basename(&tool_input.path), false)
                && names_a_file(&cwd, &tool_input.path);
            return candidate.then_some(HookCall::Write { cwd, tool_input });
        }
        HookInput::Edit { cwd, paths } => {
            let paths: Vec<String> = paths
                .iter()
                .filter(|path| names_a_file(&cwd, path))
                .map(|path| resolve(&cwd, path))
                .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn call(cwd: &str, event: &str, tool: &str, path: &str) -> Option<HookCall> {
        let input = serde_json::json!({
            "cwd": cwd,
            "hook_event_name": event,
            "tool_name": tool,
            "tool_input": { "file_path": path, "content": "" },
        });

        hook_call(hook_input(&input.to_string(), Ok("/")).unwrap())
    }

    fn edited(cwd: &str, path: &str) -> Vec<String> {
        match call(cwd, "PostToolUse", "Edit", path) {
            Some(HookCall::Edit { paths, .. }) => paths,
            _ => Vec::new(),
        }
    }

    fn writes(cwd: &str, path: &str) -> bool {
        matches!(
            call(cwd, "PreToolUse", "Write", path),
            Some(HookCall::Write { .. })
        )
    }

    #[test]
    fn hook_call_takes_only_absolute_tool_paths_under_a_missing_cwd() {
        let dir = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("stanza-hook-call-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.ts"), "").unwrap();

        let dir = dir.to_str().unwrap();
        let file = format!("{dir}/a.ts");
        let gone = format!("{dir}/gone");
        let sub = format!("{dir}/sub");
        assert_eq!(edited(&gone, &file), std::slice::from_ref(&file));
        assert!(edited(&gone, "../a.ts").is_empty());
        assert_eq!(edited(&sub, "../a.ts"), std::slice::from_ref(&file));
        assert!(writes(&gone, &file));
        assert!(!writes(&gone, "../a.ts"));
        assert!(writes(&sub, "../a.ts"));

        std::fs::remove_dir_all(dir).unwrap();
    }
}
