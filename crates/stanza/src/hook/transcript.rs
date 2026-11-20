use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::mem;

use indexmap::{IndexMap, IndexSet};
use memchr::memmem;
use serde_json::value::RawValue;

use super::input::Object;
use super::rollout::CodexRollout;
use crate::paths::{is_absolute, join};

fn each_line(path: &str, mut read: impl FnMut(&[u8])) -> Option<()> {
    let mut reader = BufReader::with_capacity(1 << 16, File::open(path).ok()?);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return Some(());
        }

        read(line.strip_suffix(b"\n").unwrap_or(&line));
    }
}

pub(super) fn written_files(transcript_path: &str) -> Option<IndexSet<String>> {
    let (format, mut written) = recorded_writes(transcript_path, Clients::ClaudeOrCodex)?;
    if format == Format::Codex {
        return Some(written);
    }

    let session = transcript_path
        .strip_suffix(".jsonl")
        .filter(|stem| !stem.ends_with('/'))
        .unwrap_or(transcript_path);

    subagent_writes(&join(session, "subagents"), &mut written);
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
            && let Some((_, subagent)) = recorded_writes(&path, Clients::Claude)
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

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Clients {
    ClaudeOrCodex,
    Claude,
}

pub(super) fn recorded_writes(path: &str, clients: Clients) -> Option<(Format, IndexSet<String>)> {
    let mut reader = Reader::new(clients);
    each_line(path, |line| reader.read(line))?;
    reader.finish()
}

enum Reading {
    Unknown {
        failures: ClaudeTranscript,
        malformed: bool,
    },
    Claude(ClaudeTranscript),
    Codex(CodexRollout),
}

struct Reader {
    clients: Clients,
    reading: Reading,
}

impl Reader {
    fn new(clients: Clients) -> Self {
        let reading = Reading::Unknown {
            failures: ClaudeTranscript::default(),
            malformed: false,
        };

        Reader { clients, reading }
    }

    fn read(&mut self, bytes: &[u8]) {
        let line = Line::new(bytes);
        match &mut self.reading {
            Reading::Claude(transcript) => {
                if line.may_hold(&[TOOL_USE, FAILURE]) {
                    match std::str::from_utf8(bytes) {
                        Ok(text) => transcript.read(text, true),
                        Err(_) => transcript.read(&failure_text(bytes), false),
                    };
                }
            }
            Reading::Codex(rollout) => {
                if line.may_hold(&[ITEM_COMPLETED])
                    && let Ok(text) = std::str::from_utf8(bytes)
                {
                    rollout.read(text);
                }
            }
            Reading::Unknown {
                failures,
                malformed,
            } => {
                let Ok(text) = std::str::from_utf8(bytes) else {
                    *malformed = true;
                    if line.may_hold(&[FAILURE]) {
                        failures.read(&failure_text(bytes), false);
                    }

                    return;
                };

                *malformed |= holds_lone_surrogate(text);
                if self.clients == Clients::ClaudeOrCodex
                    && line.may_hold(&[SESSION_META, ITEM_COMPLETED])
                {
                    let mut rollout = CodexRollout::default();
                    if rollout.read(text) {
                        self.reading = Reading::Codex(rollout);
                        return;
                    }
                }

                if line.may_hold(&[ASSISTANT, FAILURE]) && failures.read(text, true) {
                    self.reading = Reading::Claude(mem::take(failures));
                }
            }
        }
    }

    fn finish(self) -> Option<(Format, IndexSet<String>)> {
        match self.reading {
            Reading::Claude(transcript) => Some((Format::Claude, transcript.written())),
            Reading::Codex(rollout) => Some((Format::Codex, rollout.written)),
            Reading::Unknown {
                malformed: true, ..
            } => Some((Format::Claude, IndexSet::new())),
            Reading::Unknown { .. } => None,
        }
    }
}

type Pair = (&'static [u8], &'static [u8]);

const ASSISTANT: Pair = (b"\"type\"", b"\"assistant\"");
const TOOL_USE: Pair = (b"\"type\"", b"\"tool_use\"");
const FAILURE: Pair = (b"\"is_error\"", b"true");
const SESSION_META: Pair = (b"\"type\"", b"\"session_meta\"");
const ITEM_COMPLETED: Pair = (b"\"type\"", b"\"item_completed\"");

struct Line<'a> {
    bytes: &'a [u8],
    escaped: bool,
}

impl<'a> Line<'a> {
    // Only an escape from \u0040 to \u007f can spell a letter or underscore, so the \u001b that fills terminal output never hides a key.
    fn new(bytes: &'a [u8]) -> Self {
        let escaped = memmem::find_iter(bytes, b"\\u00")
            .any(|at| matches!(bytes.get(at + 4), Some(b'4'..=b'7')));

        Line { bytes, escaped }
    }

    fn may_hold(&self, pairs: &[Pair]) -> bool {
        self.escaped
            || pairs
                .iter()
                .any(|&(key, value)| has_pair(self.bytes, key, value))
    }
}

fn has_pair(line: &[u8], key: &[u8], value: &[u8]) -> bool {
    memmem::find_iter(line, key).any(|at| {
        let rest = line[at + key.len()..].trim_ascii_start();
        rest.strip_prefix(b":")
            .is_some_and(|rest| rest.trim_ascii_start().starts_with(value))
    })
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
}

impl ClaudeTranscript {
    fn read(&mut self, line: &str, writes: bool) -> bool {
        let Ok(Some(record)) = Object::parse(line) else {
            return false;
        };

        let Some(content) = record
            .value::<Object>("message")
            .and_then(|message| message.value::<Vec<&RawValue>>("content"))
        else {
            return false;
        };

        let assistant = writes && record.is("type", "assistant");
        let user = record.is("type", "user");
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

        assistant
    }

    fn written(self) -> IndexSet<String> {
        self.attempted
            .into_iter()
            .filter(|(id, _)| !self.failed.contains(id))
            .map(|(_, path)| path)
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::rollout::tests::written_files_in_codex_rollout;
    use super::*;

    pub(crate) fn read_lines(text: &str, clients: Clients) -> Option<(Format, IndexSet<String>)> {
        let mut reader = Reader::new(clients);
        text.split('\n')
            .for_each(|line| reader.read(line.as_bytes()));
        reader.finish()
    }

    pub(crate) fn transcript(lines: &[&str]) -> Option<Vec<String>> {
        read_lines(&lines.join("\n"), Clients::ClaudeOrCodex)
            .map(|(_, written)| written.into_iter().collect())
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
    fn transcripts_read_tool_calls_however_their_keys_are_spelled() {
        let written = transcript(&[
            r#"{"type":"assistant","message":{"content":[]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","type":"tool_use","id":"a","name":"Edit","input":{"file_path":"/a.ts"}}]}}"#,
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\" :\t\"tool_use\",\"id\":\"b\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/b.ts\"}}]}}",
            r#"{"type":"assistant","message":{"content":[{"t\u0079pe":"tool_use","id":"c","name":"Edit","input":{"file_path":"/c.ts"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_\u0075se","id":"d","name":"Edit","input":{"file_path":"/d.ts"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"e","name":"Edit","input":{"file_path":"/e.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"e","is_\u0065rror":true}]}}"#,
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"b\",\"is_error\" : true}]}}",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"f","name":"Edit","input":{"file_path":"/f.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"f","is\u005ferror":true}]}}"#,
        ]);

        assert_eq!(
            written,
            Some(vec![
                "/a.ts".to_owned(),
                "/c.ts".to_owned(),
                "/d.ts".to_owned()
            ])
        );
        assert_eq!(
            transcript(&[r#"{"type":"user","message":{"content":[]}}"#]),
            None
        );
    }

    #[test]
    fn an_escaped_assistant_record_identifies_a_transcript() {
        let written = transcript(&[
            r#"{"type":"\u0061ssistant","message":{"content":[{"type":"tool_use","id":"a","name":"Write","input":{"file_path":"/a.ts"}}]}}"#,
            r#"{"type":"session_meta","payload":{}}"#,
        ]);

        assert_eq!(written, Some(vec!["/a.ts".to_owned()]));
    }

    #[test]
    fn rollouts_read_completions_however_their_values_are_spelled() {
        let meta = r#"{"type":"session_meta","payload":{}}"#;
        let escaped = r#"{"type":"event_msg","payload":{"type":"item_\u0063ompleted","item":{"type":"FileChange","status":"completed","changes":{"/e.ts":{"type":"add"}}}}}"#;
        let spaced = "{\"type\":\"event_msg\",\"payload\":{\"type\" : \"item_completed\",\"item\":{\"type\":\"FileChange\",\"status\":\"completed\",\"changes\":{\"/s.ts\":{\"type\":\"add\"}}}}}";

        assert_eq!(
            streamed("escaped-codex", escaped.as_bytes(), Clients::ClaudeOrCodex),
            Some((Format::Codex, vec!["/e.ts".to_owned()]))
        );
        assert_eq!(
            streamed(
                "escaped-late-codex",
                format!("{meta}\n{spaced}\n{escaped}").as_bytes(),
                Clients::ClaudeOrCodex
            ),
            Some((Format::Codex, vec!["/s.ts".to_owned(), "/e.ts".to_owned()]))
        );
    }

    #[test]
    fn a_rollout_of_only_session_meta_is_codex_with_nothing_written() {
        for meta in [
            r#"{"type":"session_meta","payload":{}}"#,
            r#"{"type":"session_\u006deta","payload":{}}"#,
        ] {
            assert_eq!(
                streamed("meta", meta.as_bytes(), Clients::ClaudeOrCodex),
                Some((Format::Codex, vec![])),
                "{meta}"
            );
        }

        let meta = r#"{"type":"session_meta","payload":{}}"#;
        assert_eq!(
            streamed("meta-subagent", meta.as_bytes(), Clients::Claude),
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

    fn streamed(name: &str, bytes: &[u8], clients: Clients) -> Option<(Format, Vec<String>)> {
        let path = std::env::temp_dir().join(format!("stanza-{name}-{}.jsonl", std::process::id()));
        fs::write(&path, bytes).unwrap();
        let written = recorded_writes(path.to_str().unwrap(), clients);
        fs::remove_file(&path).unwrap();
        written.map(|(format, written)| (format, written.into_iter().collect()))
    }

    #[test]
    fn a_codex_line_after_a_claude_assistant_record_leaves_the_file_a_transcript() {
        let claude = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/c.ts"}}]}}"#;
        let nothing = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning"}}}"#;
        let wrote = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","status":"completed","changes":{"/w.ts":{"type":"add"}}}}}"#;
        let meta = r#"{"type":"session_meta","payload":{}}"#;

        for late in [nothing, wrote, meta] {
            assert_eq!(
                streamed(
                    "late-codex",
                    format!("{claude}\n{late}").as_bytes(),
                    Clients::ClaudeOrCodex
                ),
                Some((Format::Claude, vec!["/c.ts".to_owned()])),
                "{late}"
            );
        }

        assert_eq!(
            streamed(
                "codex",
                format!("{wrote}\n{claude}").as_bytes(),
                Clients::ClaudeOrCodex
            ),
            Some((Format::Codex, vec!["/w.ts".to_owned()]))
        );
        assert_eq!(
            streamed("claude", claude.as_bytes(), Clients::ClaudeOrCodex),
            Some((Format::Claude, vec!["/c.ts".to_owned()]))
        );
        assert_eq!(
            streamed(
                "subagent",
                format!("{claude}\n{wrote}").as_bytes(),
                Clients::Claude
            ),
            Some((Format::Claude, vec!["/c.ts".to_owned()]))
        );
        assert_eq!(
            recorded_writes("/nonexistent/stanza.jsonl", Clients::ClaudeOrCodex),
            None
        );
        assert_eq!(
            recorded_writes(
                std::env::temp_dir().to_str().unwrap(),
                Clients::ClaudeOrCodex
            ),
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
                streamed("surrogate-key", line.as_bytes(), Clients::ClaudeOrCodex),
                Some((Format::Claude, vec![])),
                "{line}"
            );
        }

        let paired = r#"{"type":"user","message":{"content":"😀 \\ud800"}}"#;
        assert_eq!(
            streamed("paired", paired.as_bytes(), Clients::ClaudeOrCodex),
            None
        );
    }

    #[test]
    fn a_failure_on_a_line_that_is_not_utf8_still_counts() {
        let lines: &[&[u8]] = &[
            br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Edit","input":{"file_path":"/a.ts"}},{"type":"tool_use","id":"b","name":"Edit","input":{"file_path":"/b.ts"}}]}}"#,
            b"{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"a\",\"is_error\":true,\"content\":\"bad \xFF\"}]}}",
            b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"c\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/c\xFF.ts\"}}]}}",
        ];

        assert_eq!(
            streamed("utf8-failure", &lines.join(&b'\n'), Clients::ClaudeOrCodex),
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
            streamed("utf8-claude", &claude.join(&b'\n'), Clients::ClaudeOrCodex),
            Some((Format::Claude, vec!["/c.ts".to_owned()]))
        );
        assert_eq!(
            streamed(
                "utf8-only",
                &claude[..3].join(&b'\n'),
                Clients::ClaudeOrCodex
            ),
            Some((Format::Claude, vec![]))
        );

        let escaped = br#"{"type":"assistant","message":{"content":[{"t\u0079pe":"tool_use","id":"e","name":"Write","input":{"file_path":"/e.ts"}}]}}"#;
        assert_eq!(
            streamed(
                "utf8-first",
                &[&claude[2][..], escaped].join(&b'\n'),
                Clients::ClaudeOrCodex
            ),
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
            streamed("utf8-codex", &codex.join(&b'\n'), Clients::ClaudeOrCodex),
            Some((Format::Codex, vec!["/e.ts".to_owned()]))
        );
        assert_eq!(
            streamed("utf8-codex-only", &codex[0], Clients::ClaudeOrCodex),
            Some((Format::Claude, vec![]))
        );
    }
}
