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
pub use input::{HookInput, hook_input};

pub const CLAUDE_CODE_HOOKS: &str = r#"{ "hooks": { "PostToolUse": [{ "matcher": "Write|Edit|MultiEdit", "hooks": [{ "type": "command", "command": "stanza hook" }] }], "Stop": [{ "hooks": [{ "type": "command", "command": "stanza hook" }] }] } }"#;

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

    fn fixed(cwd: &str, tool: &str, path: &str) -> Vec<String> {
        match call(cwd, "PostToolUse", tool, path) {
            Some(HookCall::Edit { paths, .. }) => paths,
            _ => Vec::new(),
        }
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
        for tool in ["Write", "Edit"] {
            assert_eq!(fixed(&gone, tool, &file), std::slice::from_ref(&file));
            assert!(fixed(&gone, tool, "../a.ts").is_empty());
            assert_eq!(fixed(&sub, tool, "../a.ts"), std::slice::from_ref(&file));
        }

        assert!(call(&sub, "PreToolUse", "Write", "../a.ts").is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hook_call_steps_up_from_a_symlink_as_the_tool_does() {
        let dir = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("stanza-hook-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real/sub")).unwrap();
        std::fs::write(dir.join("real/a.ts"), "").unwrap();
        std::fs::write(dir.join("a.ts"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("real/sub"), dir.join("link")).unwrap();

        let dir = dir.to_str().unwrap();
        let sibling = format!("{dir}/a.ts");
        let sibling = std::slice::from_ref(&sibling);
        for tool in ["Write", "Edit"] {
            assert_eq!(fixed(&format!("{dir}/link"), tool, "../a.ts"), sibling);
            assert_eq!(fixed(dir, tool, "link/../a.ts"), sibling);
        }

        std::fs::remove_dir_all(dir).unwrap();
    }
}
