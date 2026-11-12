use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};

use indexmap::{IndexMap, IndexSet};
use serde_json::value::RawValue;

use super::input::{Object, Text};
use super::rollout::CodexRollout;
use crate::paths::{basename, dirname, is_absolute, join};
use crate::text::is_js_whitespace;

fn each_line(path: &str, mut read: impl FnMut(&str)) -> Option<()> {
    let mut reader = BufReader::with_capacity(1 << 16, File::open(path).ok()?);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return Some(());
        }

        let text = line.strip_suffix(b"\n").unwrap_or(&line);
        read(&String::from_utf8_lossy(text));
    }
}

fn basename_without(path: &str, extension: &str) -> String {
    let name = basename(path);
    match name.strip_suffix(extension) {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => name.to_owned(),
    }
}

pub(super) fn written_files(transcript_path: &str) -> Option<IndexSet<String>> {
    let (format, mut written) = recorded_writes(transcript_path, true)?;
    if format == Format::Codex {
        return (!written.is_empty()).then_some(written);
    }

    let session = join(
        &dirname(transcript_path),
        &basename_without(transcript_path, ".jsonl"),
    );

    let subagents = join(&session, "subagents");
    let Ok(entries) = fs::read_dir(&subagents) else {
        return Some(written);
    };

    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };

        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };

        if !name.ends_with(".jsonl") {
            continue;
        }

        if let Some((_, subagent)) = recorded_writes(&join(&subagents, &name), false) {
            written.extend(subagent);
        }
    }

    Some(written)
}

#[derive(Debug, PartialEq)]
pub(super) enum Format {
    Codex,
    Claude,
}

pub(super) fn recorded_writes(path: &str, codex: bool) -> Option<(Format, IndexSet<String>)> {
    let mut rollout = codex.then(CodexRollout::default);
    let mut transcript = ClaudeTranscript::default();
    each_line(path, |line| {
        if let Some(rollout) = &mut rollout {
            rollout.read(line);
            if rollout.recognized {
                return;
            }
        }

        transcript.read(line);
    })?;

    if let Some(written) = rollout.and_then(CodexRollout::written) {
        return Some((Format::Codex, written));
    }

    transcript
        .written()
        .map(|written| (Format::Claude, written))
}

fn has_pair(line: &str, key: &str, value: &str) -> bool {
    line.match_indices(key).any(|(at, _)| {
        let rest = line[at + key.len()..].trim_start_matches(is_js_whitespace);
        rest.strip_prefix(':')
            .is_some_and(|rest| rest.trim_start_matches(is_js_whitespace).starts_with(value))
    })
}

fn edited_path(block: &Object) -> Option<(Text, String)> {
    let name = block.value::<Text>("name")?;
    if !name.is_any(&["Write", "Edit", "MultiEdit"]) {
        return None;
    }

    let id = block.value::<Text>("id")?;
    let path = block
        .value::<Object>("input")?
        .value::<Text>("file_path")?
        .lossy();
    is_absolute(&path).then_some((id, path))
}

#[derive(Default)]
struct ClaudeTranscript {
    attempted: IndexMap<Text, String>,
    failed: HashSet<Text>,
    recognized: bool,
}

impl ClaudeTranscript {
    fn read(&mut self, line: &str) {
        let tool_use = || has_pair(line, "\"type\"", "\"tool_use\"");
        let failure = || has_pair(line, "\"is_error\"", "true");
        if self.recognized && !tool_use() && !failure() {
            return;
        }

        let Ok(Some(record)) = Object::parse(line) else {
            return;
        };

        let Some(content) = record
            .value::<Object>("message")
            .and_then(|message| message.value::<Vec<&RawValue>>("content"))
        else {
            return;
        };

        let assistant = record.is("type", "assistant");
        let user = record.is("type", "user");
        self.recognized |= assistant;

        for block in content {
            let Ok(block) = serde_json::from_str::<Object>(block.get()) else {
                continue;
            };

            if assistant
                && block.is("type", "tool_use")
                && let Some((id, path)) = edited_path(&block)
            {
                self.attempted.insert(id, path);
            }

            let failed = block.is("type", "tool_result") && block.value("is_error") == Some(true);
            if user
                && failed
                && let Some(id) = block.value::<Text>("tool_use_id")
            {
                self.failed.insert(id);
            }
        }
    }

    fn written(self) -> Option<IndexSet<String>> {
        if !self.recognized {
            return None;
        }

        let written = self
            .attempted
            .into_iter()
            .filter(|(id, _)| !self.failed.contains(id))
            .map(|(_, path)| path)
            .collect();

        Some(written)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::rollout::tests::written_files_in_codex_rollout;
    use super::*;

    fn written_files_in_transcript(text: &str) -> Option<IndexSet<String>> {
        let mut transcript = ClaudeTranscript::default();
        text.split('\n').for_each(|line| transcript.read(line));
        transcript.written()
    }

    pub(crate) fn transcript(lines: &[&str]) -> Option<Vec<String>> {
        written_files_in_transcript(&lines.join("\n")).map(|written| written.into_iter().collect())
    }

    #[test]
    fn transcripts_skip_tool_calls_whose_name_or_id_is_not_a_string() {
        let written = transcript(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":["Write"],"input":{"file_path":"/a.ts"}},{"type":"tool_use","id":"t2","name":"Write","input":{"file_path":"/b.ts"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/c.ts"}},{"type":"tool_use","id":0,"name":"Edit","input":{"file_path":"/d.ts"}},{"type":"tool_use","id":"t5","name":{"toString":1},"input":{"file_path":"/e.ts"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Edit","input":{"file_path":"/f.ts"}},{"type":"tool_use","id":"t4","name":"Edit","input":{"file_path":"relative.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":0,"is_error":true},{"type":"tool_result","tool_use_id":"t3","is_error":true}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"MultiEdit","input":{"file_path":"/g.ts"}}]}}"#,
        ]);

        assert_eq!(written, Some(vec!["/b.ts".to_owned(), "/g.ts".to_owned()]));
    }

    #[test]
    fn transcripts_skip_lines_the_prefilter_rejects_once_recognized() {
        let written = transcript(&[
            r#"{"type":"assistant","message":{"content":[]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","type":"tool_use","id":"a","name":"Edit","input":{"file_path":"/a.ts"}}]}}"#,
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\" :\t\"tool_use\",\"id\":\"b\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/b.ts\"}}]}}",
            r#"{"type":"assistant","message":{"content":[{"t\u0079pe":"tool_use","id":"c","name":"Edit","input":{"file_path":"/c.ts"}}]}}"#,
        ]);

        assert_eq!(written, Some(vec!["/a.ts".to_owned(), "/b.ts".to_owned()]));
        assert_eq!(
            transcript(&[r#"{"type":"user","message":{"content":[]}}"#]),
            None
        );
    }

    #[test]
    fn ids_and_change_paths_that_differ_only_in_lone_surrogates_stay_apart() {
        let written = transcript(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"\ud800","name":"Write","input":{"file_path":"/a.ts"}},{"type":"tool_use","id":"\ud801","name":"Write","input":{"file_path":"/b.ts"}},{"type":"tool_use","id":"�","name":"Write","input":{"file_path":"/c.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"\ud801","is_error":true}]}}"#,
        ]);
        assert_eq!(written, Some(vec!["/a.ts".to_owned(), "/c.ts".to_owned()]));

        let line = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","status":"completed","changes":{"/\ud800.ts":{"type":"update","move_path":"/x.ts"},"/�.ts":{"type":"update","move_path":"/y.ts"},"/\udc00.ts":{"type":"add"}}}}}"#;
        let written: Vec<String> = written_files_in_codex_rollout(line)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(written.len(), 3);
        assert!(written.contains(&"/x.ts".to_owned()) && written.contains(&"/y.ts".to_owned()));
        assert!(written.contains(&"/\u{FFFD}.ts".to_owned()));
    }

    fn streamed(name: &str, bytes: &[u8], codex: bool) -> Option<(Format, Vec<String>)> {
        let path = std::env::temp_dir().join(format!("stanza-{name}-{}.jsonl", std::process::id()));
        fs::write(&path, bytes).unwrap();
        let written = recorded_writes(path.to_str().unwrap(), codex);
        fs::remove_file(&path).unwrap();
        written.map(|(format, written)| (format, written.into_iter().collect()))
    }

    #[test]
    fn a_codex_completion_anywhere_makes_the_file_a_rollout() {
        let claude = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/c.ts"}}]}}"#;
        let nothing = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning"}}}"#;
        let wrote = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","status":"completed","changes":{"/w.ts":{"type":"add"}}}}}"#;

        assert_eq!(
            streamed(
                "empty-codex",
                format!("{claude}\n{nothing}").as_bytes(),
                true
            ),
            Some((Format::Codex, vec![]))
        );
        assert_eq!(
            streamed("codex", format!("{claude}\n{wrote}").as_bytes(), true),
            Some((Format::Codex, vec!["/w.ts".to_owned()]))
        );
        assert_eq!(
            streamed("claude", claude.as_bytes(), true),
            Some((Format::Claude, vec!["/c.ts".to_owned()]))
        );
        assert_eq!(
            streamed("subagent", format!("{claude}\n{wrote}").as_bytes(), false),
            Some((Format::Claude, vec!["/c.ts".to_owned()]))
        );
        assert_eq!(recorded_writes("/nonexistent/stanza.jsonl", true), None);
        assert_eq!(
            recorded_writes(std::env::temp_dir().to_str().unwrap(), true),
            None
        );
    }

    #[test]
    fn streamed_lines_decode_invalid_utf8_as_the_whole_file_does() {
        let write = |id: &str, path: &[u8]| {
            let head = format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"Write","input":{{"file_path":""#
            );
            [head.as_bytes(), path, br#""}}]}}"#].concat()
        };

        let claude = [
            write("a", b"/a\xE2\x82.ts"),
            b"{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\xF0\x9F\x98".to_vec(),
            b"\xFF\xFE\x80".to_vec(),
            write("b", b"/b\xC0\xAF\xED\xA0\x80\x80.ts"),
            write("c", b"/c\xE2.ts"),
        ];
        let bytes = claude.join(&b'\n');
        let whole: Vec<String> = written_files_in_transcript(&String::from_utf8_lossy(&bytes))
            .unwrap()
            .into_iter()
            .collect();

        assert_eq!(whole[0], "/a\u{FFFD}.ts");
        assert_eq!(whole.len(), 3);
        assert_eq!(
            streamed("utf8-claude", &bytes, true),
            Some((Format::Claude, whole))
        );

        let codex: &[&[u8]] = &[
            b"{\"type\":\"session_meta\"}\xE2\x82",
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"FileChange\",\"status\":\"completed\",\"changes\":{\"/d\xE2\x82.ts\":{\"type\":\"add\"}}}}}",
            b"",
        ];
        let bytes = codex.join(&b'\n');
        let whole: Vec<String> = written_files_in_codex_rollout(&String::from_utf8_lossy(&bytes))
            .unwrap()
            .into_iter()
            .collect();

        assert_eq!(whole, ["/d\u{FFFD}.ts"]);
        assert_eq!(
            streamed("utf8-codex", &bytes, true),
            Some((Format::Codex, whole))
        );
    }
}
