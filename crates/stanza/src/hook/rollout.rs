use indexmap::IndexSet;

use super::input::Object;
use crate::paths::is_absolute;

#[derive(Default)]
pub(super) struct CodexRollout {
    pub(super) written: IndexSet<String>,
}

impl CodexRollout {
    pub(super) fn read(&mut self, line: &str) -> bool {
        let Ok(Some(record)) = Object::parse(line) else {
            return false;
        };

        if record.is("type", "session_meta") {
            return true;
        }

        let Some(payload) = record
            .value::<Object>("payload")
            .filter(|_| record.is("type", "event_msg"))
        else {
            return false;
        };

        if !payload.is("type", "item_completed") {
            return false;
        }

        let Some(item) = payload.value::<Object>("item") else {
            return true;
        };

        let file_change = item.is("type", "FileChange") && item.is("status", "completed");
        let Some(changes) = item.value::<Object>("changes").filter(|_| file_change) else {
            return true;
        };

        for (source, change) in changes.0 {
            let Ok(change) = serde_json::from_str::<Object>(change.get()) else {
                continue;
            };

            if change.is("type", "delete") {
                continue;
            }

            let Ok(moved) = change.field::<String>("move_path") else {
                continue;
            };

            let path = moved.unwrap_or(source);
            if is_absolute(&path) {
                self.written.insert(path);
            }
        }

        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::transcript::tests::read_lines;
    use super::super::transcript::{Clients, Format};
    use super::*;

    pub(crate) fn written_files_in_codex_rollout(text: &str) -> Option<IndexSet<String>> {
        match read_lines(text, Clients::ClaudeOrCodex)? {
            (Format::Codex, written) => Some(written),
            (Format::Claude, _) => None,
        }
    }

    #[test]
    fn codex_rollouts_walk_changes_in_written_order() {
        let line = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","status":"completed","changes":{"/z.ts":{"type":"add"},"/gone.ts":{"type":"delete"},"7":{"type":"update","move_path":"/moved.ts"},"rel.ts":{}}}}}"#;
        let written: Vec<String> = written_files_in_codex_rollout(line)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(written, ["/z.ts", "/moved.ts"]);
        assert_eq!(
            written_files_in_codex_rollout(r#"{"type":"event_msg"}"#),
            None
        );
    }
}
