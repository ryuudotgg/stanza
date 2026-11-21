mod block;
mod input;
mod rollout;
mod transcript;

use indexmap::IndexSet;

use crate::files::realpath;
use crate::languages::is_candidate;
use crate::paths::{basename, is_absolute, resolve};
use input::Transcript;
use transcript::{Clients, recorded_writes, written_files};

pub use block::{block_reason, reread_line};
pub use input::{HookInput, hook_input};

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
    let (cwd, transcript) = match input {
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
        HookInput::Stop { cwd, transcript } => (cwd, transcript),
    };

    let written = match transcript {
        Transcript::Session(path) => match path.and_then(|path| written_files(&path)) {
            Some(written) => written,
            None => return Some(HookCall::Stop { cwd, written: None }),
        },
        Transcript::Agent(path) => {
            let (_, written) = recorded_writes(&path?, Clients::ClaudeOrCodex)?;
            written
        }
    };

    let candidate = written.iter().any(|path| is_candidate_file(path));
    candidate.then_some(HookCall::Stop {
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

    #[test]
    fn hook_scope_policy() {
        let dir = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("stanza-hook-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.ts"), "").unwrap();
        std::fs::write(dir.join("notes.md"), "").unwrap();

        let dir = dir.to_str().unwrap();
        let claude = |path: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t","name":"Write","input":{{"file_path":"{dir}/{path}"}}}}]}}}}"#
            )
        };
        let codex = |path: &str| {
            format!(
                r#"{{"type":"session_meta","payload":{{}}}}
{{"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"FileChange","status":"completed","changes":{{"{dir}/{path}":{{"type":"add"}}}}}}}}}}"#
            )
        };
        let unrecognised = "{}\n".to_owned();
        let cases = [
            ("claude-unreadable", None, "repository"),
            (
                "claude-unrecognised",
                Some(unrecognised.clone()),
                "repository",
            ),
            ("claude-no-candidate", Some(claude("notes.md")), "none"),
            ("claude-candidate", Some(claude("a.ts")), "a.ts"),
            ("codex-unreadable", None, "repository"),
            ("codex-unrecognised", Some(unrecognised), "repository"),
            ("codex-no-candidate", Some(codex("notes.md")), "none"),
            ("codex-candidate", Some(codex("a.ts")), "a.ts"),
        ];

        let scope = |event: &str, field: &str, transcript: &str, active: bool| {
            let input = serde_json::json!({
                "cwd": dir,
                "hook_event_name": event,
                field: transcript,
                "stop_hook_active": active,
            });

            match hook_call(hook_input(&input.to_string(), Ok("/")).unwrap()) {
                None => "none".to_owned(),
                Some(HookCall::Stop { written: None, .. }) => "repository".to_owned(),
                Some(HookCall::Stop {
                    written: Some(written),
                    ..
                }) => written
                    .iter()
                    .map(|path| path.strip_prefix(&format!("{dir}/")).unwrap_or(path))
                    .collect::<Vec<_>>()
                    .join(" "),
                Some(HookCall::Edit { .. }) => "edit".to_owned(),
            }
        };

        for (name, lines, session) in cases {
            let transcript = format!("{dir}/{name}.jsonl");
            if let Some(lines) = &lines {
                std::fs::write(&transcript, lines).unwrap();
            }

            let agent = if session == "repository" {
                "none"
            } else {
                session
            };
            assert_eq!(
                scope("Stop", "transcript_path", &transcript, false),
                session,
                "Stop {name}"
            );
            assert_eq!(
                scope("SubagentStop", "agent_transcript_path", &transcript, false),
                agent,
                "SubagentStop {name}"
            );
            assert_eq!(
                scope("Stop", "transcript_path", &transcript, true),
                "none",
                "active Stop {name}"
            );
            assert_eq!(
                scope("SubagentStop", "agent_transcript_path", &transcript, true),
                "none",
                "active SubagentStop {name}"
            );
        }

        std::fs::remove_dir_all(dir).unwrap();
    }
}
