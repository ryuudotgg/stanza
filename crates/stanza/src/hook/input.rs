use std::borrow::Borrow;
use std::fmt;
use std::ops::Range;

use indexmap::IndexMap;
use serde::Deserialize;
use serde::de::{Deserializer, Visitor};
use serde_json::value::RawValue;

use crate::paths::{is_absolute, resolve};

#[derive(Debug)]
pub struct ToolInput {
    pub path: String,
    pub content: String,
    sent: String,
    content_at: Range<usize>,
}

fn span_within(outer: &str, inner: &str) -> Option<Range<usize>> {
    let start = (inner.as_ptr() as usize).checked_sub(outer.as_ptr() as usize)?;
    let span = start..start + inner.len();
    (outer.get(span.clone()) == Some(inner)).then_some(span)
}

impl ToolInput {
    fn read(sent: &str) -> Option<ToolInput> {
        let object = Object::parse(sent).ok()??;
        let path = object.value::<Text>("file_path")?.lossy();
        let content = object.value::<String>("content")?;
        let content_at = span_within(sent, object.0.get(b"content".as_slice())?.get())?;
        Some(ToolInput {
            path,
            content,
            sent: sent.to_owned(),
            content_at,
        })
    }

    pub fn sent(&self) -> &str {
        &self.sent
    }

    pub fn content_at(&self) -> Range<usize> {
        self.content_at.clone()
    }

    pub fn echo(&self, formatted: &str) -> Result<Box<RawValue>, serde_json::Error> {
        let quoted = serde_json::to_string(formatted)?;
        let before = &self.sent[..self.content_at.start];
        let after = &self.sent[self.content_at.end..];
        RawValue::from_string([before, &quoted, after].concat())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Transcript {
    Session(Option<String>),
    Agent(Option<String>),
}

#[derive(Debug)]
pub enum HookInput {
    Ignored,
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
        stop_hook_active: bool,
        transcript: Transcript,
    },
}

const HOOK_EVENTS: [&str; 4] = ["Stop", "SubagentStop", "PreToolUse", "PostToolUse"];

const POST_TOOL_USE: &[&str] = &["Edit", "MultiEdit", "apply_patch"];

const PRE_TOOL_USE: &[&str] = &["Write"];

#[derive(Debug, PartialEq, Eq, Hash)]
pub(super) struct Text(Vec<u8>);

impl Text {
    pub(super) fn is(&self, text: &str) -> bool {
        self.0 == text.as_bytes()
    }

    pub(super) fn is_any(&self, texts: &[&str]) -> bool {
        texts.iter().any(|text| self.is(text))
    }

    // serde_json gives each lone surrogate escape as 3 WTF-8 bytes.
    pub(super) fn lossy(&self) -> String {
        let mut bytes = &self.0[..];
        let mut text = String::with_capacity(bytes.len());
        loop {
            match std::str::from_utf8(bytes) {
                Ok(valid) => {
                    text.push_str(valid);
                    return text;
                }
                Err(error) => {
                    let (valid, rest) = bytes.split_at(error.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    text.push(char::REPLACEMENT_CHARACTER);
                    debug_assert_eq!(rest.first(), Some(&0xED));
                    bytes = rest.get(3..).unwrap_or_default();
                }
            }
        }
    }
}

impl Borrow<[u8]> for Text {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(TextVisitor)
    }
}

struct TextVisitor;

impl Visitor<'_> for TextVisitor {
    type Value = Text;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Text, E> {
        Ok(Text(bytes.to_vec()))
    }

    fn visit_str<E>(self, text: &str) -> Result<Text, E> {
        Ok(Text(text.as_bytes().to_vec()))
    }
}

#[derive(Deserialize)]
pub(super) struct Object<'a>(#[serde(borrow)] pub(super) IndexMap<Text, &'a RawValue>);

impl<'a> Object<'a> {
    // Keys read as bytes skip serde_json's control character check; RawValue runs it.
    pub(super) fn parse(text: &'a str) -> Result<Option<Self>, serde_json::Error> {
        let raw: &RawValue = serde_json::from_str(text)?;
        Ok(serde_json::from_str(raw.get()).ok())
    }

    pub(super) fn field<T: Deserialize<'a>>(
        &self,
        key: &str,
    ) -> Result<Option<T>, serde_json::Error> {
        match self.0.get(key.as_bytes()).copied() {
            Some(raw) => serde_json::from_str(raw.get()),
            None => Ok(None),
        }
    }

    pub(super) fn value<T: Deserialize<'a>>(&self, key: &str) -> Option<T> {
        self.field(key).ok().flatten()
    }

    pub(super) fn is(&self, key: &str, expected: &str) -> bool {
        self.value::<Text>(key)
            .is_some_and(|text| text.is(expected))
    }

    fn text(&self, key: &str) -> Result<Option<Text>, String> {
        self.field(key)
            .map_err(|_| format!("{key} must be a string"))
    }

    fn tool(&self, tools: &[&str]) -> Option<Text> {
        self.value::<Text>("tool_name")
            .filter(|name| name.is_any(tools))
    }
}

fn matches_patch_header(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("*** ")?;
    let (kind, path) = ["Add File", "Update File", "Move to"]
        .into_iter()
        .find_map(|kind| Some((kind, rest.strip_prefix(kind)?.strip_prefix(": ")?)))?;

    let terminated = path.contains(['\n', '\r', '\u{2028}', '\u{2029}']);
    (!path.is_empty() && !terminated).then_some((kind, path))
}

const PATCH_APPLIED: &str = "Success. Updated the following files:";

fn edited_paths(input: &Object) -> Vec<String> {
    let Some(tool_input) = input.value::<Object>("tool_input") else {
        return Vec::new();
    };

    let Some(tool) = input.tool(POST_TOOL_USE) else {
        return Vec::new();
    };

    if !tool.is("apply_patch") {
        return tool_input
            .value::<Text>("file_path")
            .map(|path| vec![path.lossy()])
            .unwrap_or_default();
    }

    let Some(command) = tool_input.value::<Text>("command") else {
        return Vec::new();
    };

    let applied = input
        .value::<Text>("tool_response")
        .is_some_and(|response| {
            response
                .lossy()
                .split(['\n', '\r', '\u{2028}', '\u{2029}'])
                .any(|line| line == PATCH_APPLIED)
        });

    if !applied {
        return Vec::new();
    }

    let command = command.lossy();
    let lines: Vec<&str> = command.split('\n').collect();
    let last = lines.len() - 1;
    let mut paths = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        let line = match line.strip_suffix('\r') {
            Some(line) if index < last => line,
            _ => line,
        };

        let Some((kind, path)) = matches_patch_header(line) else {
            continue;
        };

        if kind == "Move to" {
            paths.pop();
        }

        paths.push(path.to_owned());
    }

    paths
}

pub fn hook_input(text: &str, fallback_cwd: Result<&str, &str>) -> Result<HookInput, String> {
    let input = match Object::parse(text) {
        Ok(Some(input)) => input,
        Ok(None) => return Err("input must be a JSON object".to_owned()),
        Err(_) => return Err("input must be valid JSON".to_owned()),
    };

    let event = input.text("hook_event_name")?;
    if event
        .as_ref()
        .is_some_and(|event| !event.is_any(&HOOK_EVENTS))
    {
        return Ok(HookInput::Ignored);
    }

    let resolved = |path: &str| {
        let base = if is_absolute(path) {
            Ok("/")
        } else {
            fallback_cwd
        };
        base.map(|base| resolve(base, path)).map_err(str::to_owned)
    };

    let dir = match input.text("cwd")? {
        Some(cwd) => resolved(&cwd.lossy())?,
        None => fallback_cwd.map_err(str::to_owned)?.to_owned(),
    };
    let named = |name: &str| event.as_ref().is_some_and(|event| event.is(name));

    if named("PostToolUse") {
        let paths = edited_paths(&input);
        if paths.is_empty() {
            return Ok(HookInput::Ignored);
        }

        let paths = paths.iter().map(|path| resolve(&dir, path)).collect();
        return Ok(HookInput::Edit { cwd: dir, paths });
    }

    if named("PreToolUse") {
        let tool_input = input
            .value::<&RawValue>("tool_input")
            .filter(|_| input.tool(PRE_TOOL_USE).is_some())
            .and_then(|raw| ToolInput::read(raw.get()));

        return Ok(
            tool_input.map_or(HookInput::Ignored, |tool_input| HookInput::Write {
                cwd: dir,
                tool_input,
            }),
        );
    }

    let agent = named("SubagentStop");
    let field = if agent {
        "agent_transcript_path"
    } else {
        "transcript_path"
    };
    let transcript_path = input.text(field)?;
    let stop_hook_active = input
        .field::<bool>("stop_hook_active")
        .map_err(|_| "stop_hook_active must be a boolean".to_owned())?
        .unwrap_or(false);

    let path = transcript_path
        .map(|path| resolved(&path.lossy()))
        .transpose()?;
    let transcript = if agent {
        Transcript::Agent(path)
    } else {
        Transcript::Session(path)
    };

    Ok(HookInput::Stop {
        cwd: dir,
        stop_hook_active,
        transcript,
    })
}

#[cfg(test)]
mod tests {
    use super::super::transcript::tests::transcript;
    use super::*;
    use crate::usage::FLAGS;

    fn read(text: &str, cwd: &str) -> String {
        match hook_input(text, Ok(cwd)) {
            Err(error) => format!("error {error}"),
            Ok(HookInput::Ignored) => "ignored".to_owned(),
            Ok(HookInput::Write { cwd, tool_input }) => {
                let echo = tool_input.echo(&tool_input.content).unwrap();
                format!("write {cwd} {}", echo.get())
            }
            Ok(HookInput::Edit { cwd, paths }) => format!("edit {cwd} {}", paths.join(" ")),
            Ok(HookInput::Stop {
                cwd,
                stop_hook_active,
                transcript,
            }) => format!("stop {cwd} {stop_hook_active} {transcript:?}"),
        }
    }

    #[test]
    fn hook_input_defaults_absent_and_null_fields_and_ignores_other_fields() {
        for text in [
            "{}",
            r#"{"cwd":null,"stop_hook_active":null}"#,
            r#"{"event":"stop"}"#,
        ] {
            assert_eq!(read(text, "/repo"), "stop /repo false Session(None)");
        }
    }

    #[test]
    fn hook_input_resolves_a_relative_cwd_and_accepts_boolean_stop_hook_active() {
        assert_eq!(
            read(r#"{"cwd":"../other","stop_hook_active":true}"#, "/repo/sub"),
            "stop /repo/other true Session(None)"
        );
        assert_eq!(
            read(r#"{"cwd":"/other","stop_hook_active":false}"#, "/repo"),
            "stop /other false Session(None)"
        );
    }

    #[test]
    fn hook_input_resolves_a_relative_transcript_path_against_the_process_cwd() {
        assert_eq!(
            read(
                r#"{"cwd":"sub","transcript_path":"transcripts/stop.jsonl"}"#,
                "/repo"
            ),
            r#"stop /repo/sub false Session(Some("/repo/transcripts/stop.jsonl"))"#
        );
    }

    #[test]
    fn hook_input_without_a_working_directory_takes_only_absolute_paths() {
        let unreadable = "cannot read the working directory: ENOENT";
        let absolute = r#"{"cwd":"/repo","transcript_path":"/t.jsonl"}"#;
        assert!(hook_input(absolute, Err(unreadable)).is_ok());

        for text in [
            "{}",
            r#"{"cwd":"repo"}"#,
            r#"{"cwd":"/repo","transcript_path":"t.jsonl"}"#,
        ] {
            assert_eq!(
                hook_input(text, Err(unreadable)).err().as_deref(),
                Some(unreadable),
                "{text}"
            );
        }
    }

    #[test]
    fn hook_input_rejects_wrong_input_types_and_invalid_json() {
        for text in ["null", "[]", r#""input""#, "true", "1"] {
            assert_eq!(read(text, "/repo"), "error input must be a JSON object");
        }

        assert_eq!(read(r#"{"cwd":42}"#, "/repo"), "error cwd must be a string");
        assert_eq!(
            read(r#"{"transcript_path":42}"#, "/repo"),
            "error transcript_path must be a string"
        );
        assert_eq!(
            read(r#"{"stop_hook_active":"true"}"#, "/repo"),
            "error stop_hook_active must be a boolean"
        );
        assert_eq!(read("not json", "/repo"), "error input must be valid JSON");
        assert_eq!(
            read("\u{feff}{}", "/repo"),
            "error input must be valid JSON"
        );
    }

    #[test]
    fn hook_input_allowlists_stop_subagent_stop_pre_tool_use_and_post_tool_use() {
        for text in [
            r#"{"hook_event_name":"Stop"}"#,
            r#"{"hook_event_name":null}"#,
        ] {
            assert_eq!(read(text, "/repo"), "stop /repo false Session(None)");
        }

        assert_eq!(
            read(
                r#"{"hook_event_name":"SubagentStop","transcript_path":"x"}"#,
                "/repo"
            ),
            "stop /repo false Agent(None)"
        );

        for event in ["UserPromptSubmit", "PostToolUseFailure"] {
            let text = format!(r#"{{"hook_event_name":"{event}","cwd":42}}"#);
            assert_eq!(read(&text, "/repo"), "ignored");
        }

        assert_eq!(
            read(r#"{"hook_event_name":"PostToolUse","cwd":42}"#, "/repo"),
            "error cwd must be a string"
        );

        for event in ["42", "false", "{}", "[]"] {
            let text = format!(r#"{{"hook_event_name":{event}}}"#);
            assert_eq!(
                read(&text, "/repo"),
                "error hook_event_name must be a string"
            );
        }
    }

    #[test]
    fn hook_input_accepts_only_complete_write_input_for_pre_tool_use() {
        let tool_input = r#"{"file_path":"a.ts","content":"x","extra":true}"#;
        assert_eq!(
            read(
                &format!(
                    r#"{{"hook_event_name":"PreToolUse","cwd":"sub","tool_name":"Write","tool_input":{tool_input}}}"#
                ),
                "/repo"
            ),
            format!("write /repo/sub {tool_input}")
        );

        for tool in ["Edit", "MultiEdit", "Other"] {
            let text = format!(
                r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}","tool_input":{tool_input}}}"#
            );
            assert_eq!(read(&text, "/repo"), "ignored");
        }

        for tool_input in [
            "null",
            "{}",
            r#"{"file_path":"a.ts"}"#,
            r#"{"file_path":42,"content":"x"}"#,
            r#"{"file_path":"a.ts","content":42}"#,
        ] {
            let text = format!(
                r#"{{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{tool_input}}}"#
            );
            assert_eq!(read(&text, "/repo"), "ignored");
        }

        assert_eq!(
            read(r#"{"hook_event_name":"PreToolUse","cwd":42}"#, "/repo"),
            "error cwd must be a string"
        );
    }

    const APPLIED: &str = "Exit code: 0\nWall time: 0.2 seconds\nOutput:\nSuccess. Updated the following files:\nA a.ts\nM b.ts\nM c.ts\nD gone.ts\n";

    fn patch(lines: &[&str]) -> String {
        [&["*** Begin Patch"], lines, &["*** End Patch"]]
            .concat()
            .join("\n")
    }

    fn post_tool_use(tool: &str, tool_input: &str, response: Option<&str>) -> String {
        let response = response.map_or_else(String::new, |response| {
            format!(r#","tool_response":{response}"#)
        });

        format!(
            r#"{{"hook_event_name":"PostToolUse","cwd":"sub","tool_name":"{tool}","tool_input":{tool_input}{response}}}"#
        )
    }

    fn quoted(text: &str) -> String {
        serde_json::to_string(text).unwrap()
    }

    #[test]
    fn hook_input_reads_the_paths_a_post_tool_use_patch_or_edit_wrote() {
        let codex = patch(&[
            "*** Add File: a.ts",
            "+const a = 1;",
            "*** Update File: b.ts",
            "*** Update File: old.ts",
            "*** Move to: sub/../c.ts",
            "*** Delete File: gone.ts",
        ]);

        let command = format!(r#"{{"command":{}}}"#, quoted(&codex));
        assert_eq!(
            read(
                &post_tool_use("apply_patch", &command, Some(&quoted(APPLIED))),
                "/repo"
            ),
            "edit /repo/sub /repo/sub/a.ts /repo/sub/b.ts /repo/sub/c.ts"
        );

        let absolute = format!(
            r#"{{"command":{}}}"#,
            quoted(&patch(&["*** Update File: /abs/x.ts"]))
        );
        assert_eq!(
            read(
                &post_tool_use("apply_patch", &absolute, Some(&quoted(APPLIED))),
                "/repo"
            ),
            "edit /repo/sub /abs/x.ts"
        );

        for tool in ["Edit", "MultiEdit"] {
            let text = post_tool_use(
                tool,
                r#"{"file_path":"src/a.ts"}"#,
                Some(r#"{"filePath":"src/a.ts"}"#),
            );
            assert_eq!(read(&text, "/repo"), "edit /repo/sub /repo/sub/src/a.ts");
        }
    }

    #[test]
    fn hook_input_matches_patch_lines_over_javascript_line_terminators() {
        let carriage = quoted("Success. Updated the following files:\r\nA a.ts");
        let separated = quoted("x\u{2028}Success. Updated the following files:");
        let command = format!(
            r#"{{"command":{}}}"#,
            quoted(
                "*** Add File: a.ts\r\n*** Add File: b\r.ts\n*** Add File: c\u{2028}.ts\n*** Add File: \n*** Add File: d.ts\r"
            )
        );

        for response in [carriage, separated] {
            assert_eq!(
                read(
                    &post_tool_use("apply_patch", &command, Some(&response)),
                    "/repo"
                ),
                "edit /repo/sub /repo/sub/a.ts"
            );
        }
    }

    #[test]
    fn hook_input_ignores_a_failed_empty_or_foreign_post_tool_use() {
        let codex = format!(
            r#"{{"command":{}}}"#,
            quoted(&patch(&["*** Add File: a.ts"]))
        );
        let delete = format!(
            r#"{{"command":{}}}"#,
            quoted(&patch(&["*** Delete File: a.ts"]))
        );
        let applied = quoted(APPLIED);
        let cases = [
            post_tool_use(
                "apply_patch",
                &codex,
                Some(&quoted("apply_patch verification failed")),
            ),
            post_tool_use(
                "apply_patch",
                &codex,
                Some(&quoted(
                    "verification failed, expected: Success. Updated the following files:",
                )),
            ),
            post_tool_use(
                "apply_patch",
                &codex,
                Some(&format!(r#"{{"applied":{applied}}}"#)),
            ),
            post_tool_use("apply_patch", &codex, None),
            post_tool_use("apply_patch", &delete, Some(&applied)),
            post_tool_use("apply_patch", r#"{"command":42}"#, Some(&applied)),
            post_tool_use("Write", r#"{"file_path":"a.ts","content":"x"}"#, None),
            post_tool_use("Edit", r#"{"file_path":42}"#, None),
            post_tool_use("Bash", &codex, Some(&applied)),
        ];

        for text in cases {
            assert_eq!(read(&text, "/repo"), "ignored", "{text}");
        }
    }

    #[test]
    fn hook_input_keeps_the_last_value_of_a_repeated_key() {
        assert_eq!(
            read(
                r#"{"cwd":42,"cwd":"sub","stop_hook_active":"x","stop_hook_active":true}"#,
                "/repo"
            ),
            "stop /repo/sub true Session(None)"
        );
        assert_eq!(
            read(r#"{"cwd":"sub","cwd":42}"#, "/repo"),
            "error cwd must be a string"
        );
        assert_eq!(
            read(
                r#"{"hook_event_name":"Stop","hook_event_name":"Other","cwd":42}"#,
                "/repo"
            ),
            "ignored"
        );
        assert_eq!(
            read(
                r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_name":"Write","tool_input":{"file_path":"b.ts"},"tool_input":{"file_path":"a.ts","content":"x","file_path":"c.ts"}}"#,
                "/repo"
            ),
            r#"write /repo {"file_path":"a.ts","content":"x","file_path":"c.ts"}"#
        );
        assert_eq!(
            read(
                &post_tool_use("Edit", r#"{"file_path":42,"file_path":"a.ts"}"#, None),
                "/repo"
            ),
            "edit /repo/sub /repo/sub/a.ts"
        );
    }

    #[test]
    fn hook_input_reads_a_lone_surrogate_as_a_replacement_character() {
        assert_eq!(
            read(
                r#"{"\ud800":1,"cwd":"/a\ud800b\udc00","x":{"\udc00":[]}}"#,
                "/repo"
            ),
            "stop /a\u{FFFD}b\u{FFFD} false Session(None)"
        );
        assert_eq!(
            read(
                &post_tool_use("Edit", r#"{"file_path":"😀\ud83d.ts"}"#, None),
                "/repo"
            ),
            "edit /repo/sub /repo/sub/\u{1F600}\u{FFFD}.ts"
        );
    }

    #[test]
    fn hook_input_ignores_tool_fields_of_another_type() {
        let write = r#"{"file_path":"a.ts","content":"x"}"#;
        for text in [
            format!(r#"{{"hook_event_name":"PreToolUse","tool_name":42,"tool_input":{write}}}"#),
            format!(
                r#"{{"hook_event_name":"PreToolUse","tool_name":["Write"],"tool_input":{write}}}"#
            ),
            r#"{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":"x"}"#.to_owned(),
            post_tool_use("Edit", "[]", None),
            post_tool_use("Edit", r#"{"file_path":"a.ts"}"#, None).replace(r#""Edit""#, "42"),
            post_tool_use(
                "apply_patch",
                &format!(
                    r#"{{"command":{}}}"#,
                    quoted(&patch(&["*** Add File: a.ts"]))
                ),
                Some("42"),
            ),
        ] {
            assert_eq!(read(&text, "/repo"), "ignored", "{text}");
        }
    }

    #[test]
    fn hook_input_rejects_a_raw_control_character_in_a_key() {
        for text in [
            "{\"\t\":1,\"hook_event_name\":\"Bogus\"}",
            "{\"c\nwd\":1}",
            "{\"\u{1}\":1,\"hook_event_name\":\"Stop\"}",
        ] {
            assert_eq!(
                read(text, "/repo"),
                "error input must be valid JSON",
                "{text:?}"
            );
        }

        assert_eq!(
            transcript(&[
                "{\"\u{1}\":0,\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"Write\",\"input\":{\"file_path\":\"/a.ts\"}}]}}"
            ]),
            None
        );
    }

    #[test]
    fn hook_input_reads_past_wrong_types_in_fields_its_event_never_uses() {
        let write = r#""tool_name":"Write","tool_input":{"file_path":"a.ts","content":"x"}"#;
        let edit = r#""tool_name":"Edit","tool_input":{"file_path":"a.ts"}"#;
        for (text, expected) in [
            (
                format!(r#"{{"hook_event_name":"PreToolUse",{write},"transcript_path":42,"stop_hook_active":"x","agent_transcript_path":[]}}"#),
                r#"write /repo {"file_path":"a.ts","content":"x"}"#,
            ),
            (
                format!(r#"{{"hook_event_name":"PostToolUse",{edit},"transcript_path":{{}},"stop_hook_active":"x"}}"#),
                "edit /repo /repo/a.ts",
            ),
            (
                r#"{"hook_event_name":"Stop","tool_input":7,"tool_name":{},"tool_response":[],"agent_transcript_path":42}"#.to_owned(),
                "stop /repo false Session(None)",
            ),
            (
                r#"{"hook_event_name":"SubagentStop","transcript_path":42,"tool_input":"x"}"#.to_owned(),
                "stop /repo false Agent(None)",
            ),
        ] {
            assert_eq!(read(&text, "/repo"), expected, "{text}");
        }
    }

    fn write_input(tool_input: &str) -> Option<ToolInput> {
        let text = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{tool_input}}}"#
        );

        match hook_input(&text, Ok("/repo")) {
            Ok(HookInput::Write { tool_input, .. }) => Some(tool_input),
            _ => None,
        }
    }

    #[test]
    fn the_echo_replaces_only_the_last_content_value_and_keeps_every_other_byte() {
        let sent = r#"{ "content" : "draft", "n":[1e400,-0,1.0], "\ud800":"\u00e9", "content" :"x\u000ay\/" ,"file_path":"a.ts"}"#;
        let tool_input = write_input(sent).unwrap();
        assert_eq!(tool_input.content, "x\ny/");
        assert_eq!(&sent[tool_input.content_at()], r#""x\u000ay\/""#);
        assert_eq!(
            tool_input.echo("a \"b\"\t\u{1}\u{2028}").unwrap().get(),
            format!(
                r#"{{ "content" : "draft", "n":[1e400,-0,1.0], "\ud800":"\u00e9", "content" :"a \"b\"\t\u0001{}" ,"file_path":"a.ts"}}"#,
                '\u{2028}'
            )
        );
    }

    #[test]
    fn a_write_whose_content_holds_a_lone_surrogate_is_ignored() {
        for content in [r#""\ud800""#, r#""a\udc00b""#, r#""\ude00\ud83d""#] {
            let sent = format!(r#"{{"file_path":"a.ts","content":{content}}}"#);
            assert!(write_input(&sent).is_none(), "{content}");
        }

        let paired = write_input(r#"{"file_path":"\ud800.ts","content":"\ud83d\ude00"}"#).unwrap();
        assert_eq!(paired.content, "\u{1F600}");
        assert_eq!(paired.path, "\u{FFFD}.ts");
    }

    fn documented_tools() -> Vec<(String, Vec<String>)> {
        let description = FLAGS.iter().find(|[name, _]| *name == "hook").unwrap()[1];
        let clauses: Vec<&str> = description.split("; ").collect();
        clauses[..clauses.len() - 1]
            .iter()
            .flat_map(|clause| {
                let (events, tools) = clause.split_once(" on ").unwrap_or((clause, ""));
                let mut names: Vec<String> = tools
                    .split(", ")
                    .flat_map(|part| part.split(" and "))
                    .filter(|part| !part.is_empty())
                    .map(|part| part.rsplit(' ').next().unwrap().replace('`', ""))
                    .collect();

                names.sort();
                events
                    .split(" and ")
                    .map(move |event| (event.to_owned(), names.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn tools_on(event: &str) -> &'static [&'static str] {
        match event {
            "PostToolUse" => POST_TOOL_USE,
            "PreToolUse" => PRE_TOOL_USE,
            _ => &[],
        }
    }

    fn accepts(event: &str, tool: &str) -> bool {
        let text = format!(
            r#"{{"hook_event_name":"{event}","tool_name":"{tool}","tool_input":{{"file_path":"example.ts","notebook_path":"example.ipynb","content":"","command":"*** Update File: example.ts"}},"tool_response":"Success. Updated the following files:\nM example.ts"}}"#
        );

        let input = hook_input(&text, Ok("/repo")).expect("valid hook input");
        !matches!(input, HookInput::Ignored)
    }

    #[test]
    fn help_hook_row_names_every_event_the_hook_accepts() {
        let mut documented: Vec<String> = documented_tools()
            .into_iter()
            .map(|(event, _)| event)
            .collect();
        let mut events = HOOK_EVENTS.map(str::to_owned).to_vec();
        documented.sort();
        events.sort();
        assert_eq!(documented, events);

        for event in HOOK_EVENTS {
            assert!(
                accepts(event, tools_on(event).first().copied().unwrap_or("")),
                "{event}"
            );
        }

        let description = FLAGS.iter().find(|[name, _]| *name == "hook").unwrap()[1];
        assert!(description.ends_with("reads JSON on stdin"));
    }

    #[test]
    fn help_hook_row_names_every_tool_the_hook_accepts_on_each_event() {
        for (event, documented) in documented_tools() {
            let mut tools: Vec<String> = tools_on(&event)
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect();
            tools.sort();
            assert_eq!(documented, tools, "{event}");
        }
    }

    #[test]
    fn hook_input_accepts_exactly_the_listed_tools() {
        let candidates = [
            POST_TOOL_USE,
            PRE_TOOL_USE,
            &["NotebookEdit", "Bash", "Read"],
        ]
        .concat();
        for event in ["PostToolUse", "PreToolUse"] {
            for tool in &candidates {
                assert_eq!(
                    accepts(event, tool),
                    tools_on(event).contains(tool),
                    "{tool} on {event}"
                );
            }
        }
    }
}
