use indexmap::IndexSet;

use super::input::{Object, Text};
use crate::paths::is_absolute;

#[derive(Default)]
pub(super) struct CodexRollout {
    written: IndexSet<String>,
    pub(super) recognized: bool,
}

impl CodexRollout {
    pub(super) fn read(&mut self, line: &str) {
        if !line.contains("\"item_completed\"") {
            return;
        }

        let Ok(Some(record)) = Object::parse(line) else {
            return;
        };

        let Some(payload) = record
            .value::<Object>("payload")
            .filter(|_| record.is("type", "event_msg"))
        else {
            return;
        };

        if !payload.is("type", "item_completed") {
            return;
        }

        self.recognized = true;

        let Some(item) = payload.value::<Object>("item") else {
            return;
        };

        let file_change = item.is("type", "FileChange") && item.is("status", "completed");
        let Some(changes) = item.value::<Object>("changes").filter(|_| file_change) else {
            return;
        };

        for (source, change) in changes.0 {
            let Ok(change) = serde_json::from_str::<Object>(change.get()) else {
                continue;
            };

            if change.is("type", "delete") {
                continue;
            }

            let path = change.value::<Text>("move_path").unwrap_or(source).lossy();
            if is_absolute(&path) {
                self.written.insert(path);
            }
        }
    }

    pub(super) fn written(self) -> Option<IndexSet<String>> {
        self.recognized.then_some(self.written)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn written_files_in_codex_rollout(text: &str) -> Option<IndexSet<String>> {
        let mut rollout = CodexRollout::default();
        text.split('\n').for_each(|line| rollout.read(line));
        rollout.written()
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
