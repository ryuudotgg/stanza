use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};

use indexmap::{IndexMap, IndexSet};
use serde_json::value::RawValue;

use super::input::Object;
use super::rollout::CodexRollout;
use crate::paths::{basename, dirname, is_absolute, join};
use crate::text::is_js_whitespace;

fn each_line(path: &str, mut read: impl FnMut(Result<&str, &[u8]>)) -> Option<()> {
    let mut reader = BufReader::with_capacity(1 << 16, File::open(path).ok()?);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return Some(());
        }

        let bytes = line.strip_suffix(b"\n").unwrap_or(&line);
        read(std::str::from_utf8(bytes).map_err(|_| bytes));
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
        return Some(written);
    }

    let session = join(
        dirname(transcript_path),
        &basename_without(transcript_path, ".jsonl"),
    );

    subagent_writes(&join(&session, "subagents"), &mut written);
    Some(written)
}

fn subagent_writes(dir: &str, written: &mut IndexSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };

        let path = join(dir, &name);
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            subagent_writes(&path, written);
        } else if name.ends_with(".jsonl")
            && let Some((_, subagent)) = recorded_writes(&path, false)
        {
            written.extend(subagent);
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Format {
    Codex,
    Claude,
}

pub(super) fn recorded_writes(path: &str, codex: bool) -> Option<(Format, IndexSet<String>)> {
    let mut rollout = codex.then(CodexRollout::default);
    let mut transcript = ClaudeTranscript::default();
    let mut malformed = false;
    each_line(path, |line| {
        let line = match line {
            Ok(line) => line,
            Err(bytes) => {
                malformed = true;
                transcript.read(&failure_text(bytes), false);
                return;
            }
        };

        malformed |= holds_lone_surrogate(line);
        if let Some(rollout) = &mut rollout {
            rollout.read(line);
            if rollout.recognized {
                return;
            }
        }

        transcript.read(line, true);
    })?;

    if let Some(written) = rollout.and_then(CodexRollout::written) {
        return Some((Format::Codex, written));
    }

    match transcript.written() {
        Some(written) => Some((Format::Claude, written)),
        None => malformed.then(|| (Format::Claude, IndexSet::new())),
    }
}

// Not a path decoder: a failure id read through a replacement character can only drop a write.
fn failure_text(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            text.push(char::REPLACEMENT_CHARACTER);
        }
    }

    text
}

fn holds_lone_surrogate(line: &str) -> bool {
    let escaped = line.match_indices("\\u").any(|(at, _)| {
        let hex = line.as_bytes().get(at + 2..at + 4);
        matches!(
            hex,
            Some([b'd' | b'D', b'8'..=b'9' | b'a'..=b'f' | b'A'..=b'F'])
        )
    });

    escaped
        && serde_json::from_str::<&RawValue>(line).is_ok()
        && serde_json::from_str::<serde_json::Value>(line).is_err()
}

fn has_pair(line: &str, key: &str, value: &str) -> bool {
    line.match_indices(key).any(|(at, _)| {
        let rest = line[at + key.len()..].trim_start_matches(is_js_whitespace);
        rest.strip_prefix(':')
            .is_some_and(|rest| rest.trim_start_matches(is_js_whitespace).starts_with(value))
    })
}

fn edited_path(block: &Object) -> Option<(String, String)> {
    let name = block.value::<String>("name")?;
    if !["Write", "Edit", "MultiEdit"].contains(&name.as_str()) {
        return None;
    }

    let id = block.value::<String>("id")?;
    let path = block
        .value::<Object>("input")?
        .value::<String>("file_path")?;
    is_absolute(&path).then_some((id, path))
}

#[derive(Default)]
struct ClaudeTranscript {
    attempted: IndexMap<String, String>,
    failed: HashSet<String>,
    recognized: bool,
}

impl ClaudeTranscript {
    fn read(&mut self, line: &str, writes: bool) {
        let tool_use = || writes && has_pair(line, "\"type\"", "\"tool_use\"");
        let failure = || has_pair(line, "\"is_error\"", "true");
        if (self.recognized || !writes) && !tool_use() && !failure() {
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

        let assistant = writes && record.is("type", "assistant");
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
                && let Some(id) = block.value::<String>("tool_use_id")
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
        text.split('\n')
            .for_each(|line| transcript.read(line, true));
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
    fn lone_surrogates_in_ids_and_paths_name_no_write() {
        let written = transcript(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"\ud800","name":"Write","input":{"file_path":"/a.ts"}},{"type":"tool_use","id":"b","name":"Write","input":{"file_path":"/b\udc00.ts"}},{"type":"tool_use","id":"�","name":"Write","input":{"file_path":"/c.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"\ud801","is_error":true}]}}"#,
        ]);
        assert_eq!(written, Some(vec!["/c.ts".to_owned()]));

        let changes = |changes: &str| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"FileChange","status":"completed","changes":{{{changes}}}}}}}}}"#
            )
        };
        let lines = [
            changes(r#""/\ud800.ts":{"type":"add"},"/a.ts":{"type":"add"}"#),
            changes(
                r#""/�.ts":{"type":"add"},"/b.ts":{"type":"update","move_path":"/\udc00.ts"},"/c.ts":{"type":"update","move_path":null},"/d.ts":{"type":"update","move_path":42}"#,
            ),
        ];
        let written: Vec<String> = written_files_in_codex_rollout(&lines.join("\n"))
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(written, ["/\u{FFFD}.ts", "/c.ts"]);
    }

    #[test]
    fn a_malformed_content_block_beside_a_valid_one_still_counts_the_valid_one() {
        let written = transcript(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Write","input":{"file_path":"/a.ts"},"\ud800":1},{"type":"tool_use","id":"b","name":"Edit","input":{"file_path":"/b.ts"}},{"type":"tool_use","id":"c","name":"Edit","input":{"file_path":"/c.ts","\udc00":0}}]}}"#,
        ]);

        assert_eq!(written, Some(vec!["/b.ts".to_owned()]));
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
    fn a_record_with_a_lone_surrogate_key_keeps_the_fallback_off() {
        for line in [
            r#"{"type":"assistant","\ud800":1,"message":{"content":[]}}"#,
            r#"{"type":"assistant","message":{"\udc00":1,"content":[]}}"#,
        ] {
            assert_eq!(
                streamed("surrogate-key", line.as_bytes(), true),
                Some((Format::Claude, vec![])),
                "{line}"
            );
        }

        let paired = r#"{"type":"user","message":{"content":"😀 \\ud800"}}"#;
        assert_eq!(streamed("paired", paired.as_bytes(), true), None);
    }

    #[test]
    fn a_failure_on_a_line_that_is_not_utf8_still_counts() {
        let lines: &[&[u8]] = &[
            br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Edit","input":{"file_path":"/a.ts"}},{"type":"tool_use","id":"b","name":"Edit","input":{"file_path":"/b.ts"}}]}}"#,
            b"{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"a\",\"is_error\":true,\"content\":\"bad \xFF\"}]}}",
            b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"c\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/c\xFF.ts\"}}]}}",
        ];

        assert_eq!(
            streamed("utf8-failure", &lines.join(&b'\n'), true),
            Some((Format::Claude, vec!["/b.ts".to_owned()]))
        );
    }

    #[test]
    fn streamed_lines_that_are_not_utf8_add_no_write() {
        let write = |id: &str, path: &[u8]| {
            let head = format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"Write","input":{{"file_path":""#
            );
            [head.as_bytes(), path, br#""}}]}}"#].concat()
        };

        let claude = [
            write("a", b"/a\xE2\x82.ts"),
            b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\xF0\x9F\x98".to_vec(),
            b"\xFF\xFE\x80".to_vec(),
            write("b", b"/b\xC0\xAF\xED\xA0\x80\x80.ts"),
            write("c", b"/c.ts"),
        ];

        assert_eq!(
            streamed("utf8-claude", &claude.join(&b'\n'), true),
            Some((Format::Claude, vec!["/c.ts".to_owned()]))
        );
        assert_eq!(
            streamed("utf8-only", &claude[..3].join(&b'\n'), true),
            Some((Format::Claude, vec![]))
        );

        let escaped = br#"{"type":"assistant","message":{"content":[{"t\u0079pe":"tool_use","id":"e","name":"Write","input":{"file_path":"/e.ts"}}]}}"#;
        assert_eq!(
            streamed("utf8-first", &[&claude[2][..], escaped].join(&b'\n'), true),
            Some((Format::Claude, vec!["/e.ts".to_owned()]))
        );

        let change = |path: &[u8]| {
            [
                br#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","status":"completed","changes":{""#.as_slice(),
                path,
                br#"":{"type":"add"}}}}}"#,
            ]
            .concat()
        };
        let codex = [change(b"/d\xE2\x82.ts"), change(b"/e.ts")];

        assert_eq!(
            streamed("utf8-codex", &codex.join(&b'\n'), true),
            Some((Format::Codex, vec!["/e.ts".to_owned()]))
        );
        assert_eq!(
            streamed("utf8-codex-only", &codex[0], true),
            Some((Format::Claude, vec![]))
        );
    }
}
