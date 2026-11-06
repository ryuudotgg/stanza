use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;

use indexmap::{IndexMap, IndexSet};

use crate::engine::rules::{RULES, Rule};
use crate::engine::types::{Braces, Finding};
use crate::files::realpath;
use crate::json::{JsObject, JsString, Json, MapKey, js_string, parse};
use crate::languages::is_candidate;
use crate::paths::{basename, dirname, is_absolute, join, resolve};
use crate::text::is_js_whitespace;
use crate::usage::BOTH_BRACES;

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

#[derive(Debug, PartialEq, Eq)]
pub struct HookFlags {
    pub braces: Option<Braces>,
    pub hunks: bool,
}

const SWITCHES: [&str; 3] = ["--braces", "--hunks", "--no-braces"];

pub fn hook_flags(args: &[String]) -> Result<HookFlags, String> {
    let unexpected = args.iter().enumerate().find(|(index, arg)| {
        !SWITCHES.contains(&arg.as_str())
            || args.iter().position(|other| other == *arg) != Some(*index)
    });

    if let Some((_, arg)) = unexpected {
        return Err(format!("unexpected argument {arg}"));
    }

    let given = |flag: &str| args.iter().any(|arg| arg == flag);
    if given("--braces") && given("--no-braces") {
        return Err(BOTH_BRACES.to_owned());
    }

    Ok(HookFlags {
        braces: if given("--braces") {
            Some(Braces::On)
        } else if given("--no-braces") {
            Some(Braces::Off)
        } else {
            None
        },
        hunks: given("--hunks"),
    })
}

#[derive(Debug)]
pub struct ToolInput {
    object: JsObject,
    pub file_path: JsString,
    pub content: JsString,
}

const MASK: char = '\u{2065}';

pub struct Masked {
    pub text: String,
    originals: Vec<u16>,
}

impl ToolInput {
    pub fn path(&self) -> String {
        self.file_path.lossy().into_owned()
    }

    pub fn masked(&self) -> Masked {
        let JsString::Units(units) = &self.content else {
            return Masked {
                text: self.content.lossy().into_owned(),
                originals: Vec::new(),
            };
        };

        let mut text = String::new();
        let mut originals = Vec::new();
        for decoded in char::decode_utf16(units.iter().copied()) {
            match decoded {
                Ok(MASK) => originals.push(MASK as u16),
                Ok(char) => {
                    text.push(char);
                    continue;
                }
                Err(error) => originals.push(error.unpaired_surrogate()),
            }

            text.push(MASK);
        }

        Masked { text, originals }
    }

    pub fn updated(&self, masked: &Masked, fixed: String) -> Option<Json> {
        if masked.originals.is_empty() {
            return Some(self.with_content(JsString::Text(fixed.into())));
        }

        if fixed.matches(MASK).count() != masked.originals.len() {
            return None;
        }

        let mut restored = masked.originals.iter();
        let mut units = Vec::new();
        for char in fixed.chars() {
            if char != MASK {
                units.extend(char.encode_utf16(&mut [0; 2]).iter());
            } else if let Some(unit) = restored.next() {
                units.push(*unit);
            }
        }

        Some(self.with_content(JsString::Units(units.into())))
    }

    fn with_content(&self, content: JsString) -> Json {
        let mut object = self.object.clone();
        object.insert(JsString::from("content"), Json::String(content));
        Json::Object(object.into())
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

const HOOK_EVENTS: [&str; 4] = ["Stop", "SubagentStop", "PreToolUse", "PostToolUse"];

const POST_TOOL_USE: &[&str] = &["Edit", "MultiEdit", "apply_patch"];

const PRE_TOOL_USE: &[&str] = &["Write"];

fn present<'a>(input: &'a JsObject, key: &str) -> Option<&'a Json> {
    input.get(key).filter(|value| !matches!(value, Json::Null))
}

fn optional_text<'a>(input: &'a JsObject, key: &str) -> Result<Option<&'a JsString>, String> {
    match present(input, key) {
        None => Ok(None),
        Some(Json::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn uses_tool(input: &JsObject, tools: &[&str]) -> bool {
    input
        .text("tool_name")
        .is_some_and(|name| tools.iter().any(|tool| name.is(tool)))
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

fn edited_paths(input: &JsObject) -> Vec<String> {
    let Some(tool_input) = input.record("tool_input") else {
        return Vec::new();
    };

    if !uses_tool(input, POST_TOOL_USE) {
        return Vec::new();
    }

    if !input
        .text("tool_name")
        .is_some_and(|name| name.is("apply_patch"))
    {
        return tool_input
            .text("file_path")
            .map(|path| vec![path.lossy().into_owned()])
            .unwrap_or_default();
    }

    let Some(command) = tool_input.text("command") else {
        return Vec::new();
    };

    let applied = input.text("tool_response").is_some_and(|response| {
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

pub fn hook_input(text: &str, fallback_cwd: &str) -> Result<HookInput, String> {
    let Some(input) = parse(text) else {
        return Err("input must be valid JSON".to_owned());
    };

    let Json::Object(input) = input else {
        return Err("input must be a JSON object".to_owned());
    };

    let event = match present(&input, "hook_event_name") {
        None => None,
        Some(Json::String(event)) => Some(event),
        Some(_) => return Err("hook_event_name must be a string".to_owned()),
    };

    if event.is_some_and(|event| !HOOK_EVENTS.iter().any(|known| event.is(known))) {
        return Ok(HookInput::Ignored);
    }

    let cwd = optional_text(&input, "cwd")?;
    let dir = cwd.map_or_else(
        || fallback_cwd.to_owned(),
        |cwd| resolve(fallback_cwd, &cwd.lossy()),
    );
    let named = |name: &str| event.is_some_and(|event| event.is(name));

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
            .record("tool_input")
            .filter(|_| uses_tool(&input, PRE_TOOL_USE));
        let fields = tool_input.and_then(|tool_input| {
            Some((
                tool_input,
                tool_input.text("file_path")?,
                tool_input.text("content")?,
            ))
        });

        let Some((object, file_path, content)) = fields else {
            return Ok(HookInput::Ignored);
        };

        let tool_input = ToolInput {
            object: object.clone(),
            file_path: file_path.clone(),
            content: content.clone(),
        };

        return Ok(HookInput::Write {
            cwd: dir,
            tool_input,
        });
    }

    let agent = named("SubagentStop");
    let field = if agent {
        "agent_transcript_path"
    } else {
        "transcript_path"
    };
    let transcript_path = optional_text(&input, field)?;
    let stop_hook_active = match present(&input, "stop_hook_active") {
        None => false,
        Some(Json::Bool(active)) => *active,
        Some(_) => return Err("stop_hook_active must be a boolean".to_owned()),
    };

    let path = transcript_path.map(|path| resolve(fallback_cwd, &path.lossy()));
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

fn is_candidate_file(path: &str) -> bool {
    realpath(path).is_ok_and(|real| is_candidate(basename(&real), false))
}

fn read_lossy(path: &str) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

pub fn hook_call(input: HookInput) -> Result<Option<HookCall>, String> {
    let (cwd, stop_hook_active, transcript) = match input {
        HookInput::Ignored => return Ok(None),
        HookInput::Write { cwd, tool_input } => {
            let candidate = is_candidate(basename(&tool_input.path()), false);
            return Ok(candidate.then_some(HookCall::Write { cwd, tool_input }));
        }
        HookInput::Edit { cwd, paths } => {
            let candidate = paths.iter().any(|path| is_candidate_file(path));
            return Ok(candidate.then_some(HookCall::Edit { cwd, paths }));
        }
        HookInput::Stop {
            cwd,
            stop_hook_active,
            transcript,
        } => (cwd, stop_hook_active, transcript),
    };

    if stop_hook_active {
        return Ok(None);
    }

    let written = match transcript {
        Transcript::Agent(None) => return Ok(None),
        Transcript::Agent(Some(path)) => {
            let Some(text) = read_lossy(&path) else {
                return Ok(None);
            };

            let written = match written_files_in_codex_rollout(&text) {
                Some(written) => Some(written),
                None => written_files_in_transcript(&text)?,
            };

            match written {
                Some(written) if written.iter().any(|path| is_candidate_file(path)) => written,
                _ => return Ok(None),
            }
        }
        Transcript::Session(path) => {
            let written = match path {
                Some(path) => written_files(&path)?,
                None => None,
            };

            if written
                .as_ref()
                .is_some_and(|written| !written.iter().any(|path| is_candidate_file(path)))
            {
                return Ok(None);
            }

            return Ok(Some(HookCall::Stop { cwd, written }));
        }
    };

    Ok(Some(HookCall::Stop {
        cwd,
        written: Some(written),
    }))
}

fn basename_without(path: &str, extension: &str) -> String {
    let name = basename(path);
    match name.strip_suffix(extension) {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => name.to_owned(),
    }
}

pub fn written_files(transcript_path: &str) -> Result<Option<IndexSet<String>>, String> {
    let Some(text) = read_lossy(transcript_path) else {
        return Ok(None);
    };

    if let Some(codex) = written_files_in_codex_rollout(&text) {
        return Ok((!codex.is_empty()).then_some(codex));
    }

    let Some(mut written) = written_files_in_transcript(&text)? else {
        return Ok(None);
    };

    let session = join(
        &dirname(transcript_path),
        &basename_without(transcript_path, ".jsonl"),
    );

    let subagents = join(&session, "subagents");
    let Ok(entries) = fs::read_dir(&subagents) else {
        return Ok(Some(written));
    };

    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };

        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") {
            continue;
        }

        let Some(text) = read_lossy(&join(&subagents, &name)) else {
            continue;
        };

        if let Some(subagent) = written_files_in_transcript(&text)? {
            written.extend(subagent);
        }
    }

    Ok(Some(written))
}

fn written_files_in_codex_rollout(text: &str) -> Option<IndexSet<String>> {
    let mut written = IndexSet::new();
    let mut recognized = false;
    for line in text.split('\n') {
        if !line.contains("\"item_completed\"") {
            continue;
        }

        let Some(Json::Object(record)) = parse(line) else {
            continue;
        };

        let event = record.text("type").is_some_and(|kind| kind.is("event_msg"));
        let Some(payload) = record.record("payload").filter(|_| event) else {
            continue;
        };

        if !payload
            .text("type")
            .is_some_and(|kind| kind.is("item_completed"))
        {
            continue;
        }

        recognized = true;

        let Some(item) = payload.record("item") else {
            continue;
        };

        let file_change = item.text("type").is_some_and(|kind| kind.is("FileChange"));
        let completed = item
            .text("status")
            .is_some_and(|status| status.is("completed"));
        let Some(changes) = item.record("changes").filter(|_| file_change && completed) else {
            continue;
        };

        for (source, change) in changes.entries() {
            let Json::Object(change) = change else {
                continue;
            };

            if change.text("type").is_some_and(|kind| kind.is("delete")) {
                continue;
            }

            let path = match change.text("move_path") {
                Some(moved) => moved.lossy().into_owned(),
                None => source.lossy().into_owned(),
            };

            if is_absolute(&path) {
                written.insert(path);
            }
        }
    }

    recognized.then_some(written)
}

fn has_pair(line: &str, key: &str, value: &str) -> bool {
    line.match_indices(key).any(|(at, _)| {
        let rest = line[at + key.len()..].trim_start_matches(is_js_whitespace);
        rest.strip_prefix(':')
            .is_some_and(|rest| rest.trim_start_matches(is_js_whitespace).starts_with(value))
    })
}

fn edited_path(item: &JsObject) -> Result<Option<String>, String> {
    if !item.text("type").is_some_and(|kind| kind.is("tool_use")) {
        return Ok(None);
    }

    let name = js_string(item.get("name")).map_err(str::to_owned)?;

    if !matches!(&*name, "Write" | "Edit" | "MultiEdit") {
        return Ok(None);
    }

    let path = item
        .record("input")
        .and_then(|input| input.text("file_path"))
        .map(JsString::lossy);

    Ok(path.filter(|path| is_absolute(path)).map(Cow::into_owned))
}

fn written_files_in_transcript(text: &str) -> Result<Option<IndexSet<String>>, String> {
    let mut attempted: IndexMap<MapKey, String> = IndexMap::new();
    let mut failed: HashSet<MapKey> = HashSet::new();
    let mut recognized = false;
    for line in text.split('\n') {
        let tool_use = || has_pair(line, "\"type\"", "\"tool_use\"");
        let failure = || has_pair(line, "\"is_error\"", "true");
        if recognized && !tool_use() && !failure() {
            continue;
        }

        let Some(Json::Object(record)) = parse(line) else {
            continue;
        };

        let Some(message) = record.record("message") else {
            continue;
        };

        let Some(Json::Array(content)) = message.get("content") else {
            continue;
        };

        let assistant = record.text("type").is_some_and(|kind| kind.is("assistant"));
        let user = record.text("type").is_some_and(|kind| kind.is("user"));
        recognized |= assistant;

        for item in content.iter() {
            let Json::Object(item) = item else {
                continue;
            };

            let path = edited_path(item)?;
            if let Some(path) = path.filter(|_| assistant) {
                attempted.insert(MapKey::of(item.get("id")), path);
            }

            let result = item.text("type").is_some_and(|kind| kind.is("tool_result"));
            if user && result && matches!(item.get("is_error"), Some(Json::Bool(true))) {
                failed.insert(MapKey::of(item.get("tool_use_id")));
            }
        }
    }

    if !recognized {
        return Ok(None);
    }

    let written = attempted
        .into_iter()
        .filter(|(id, _)| !failed.contains(id))
        .map(|(_, path)| path)
        .collect();

    Ok(Some(written))
}

fn failure(rule: Rule) -> Option<&'static str> {
    match rule {
        Rule::Parse => Some("could not read or parse this file"),
        Rule::Error => Some("stanza failed on this file"),
        Rule::Write => Some("could not write the fixes to this file"),
        _ => None,
    }
}

pub fn block_reason(findings: &[Finding], rewritten: &[String]) -> Option<String> {
    if findings.is_empty() {
        return None;
    }

    let shown = &findings[..findings.len().min(12)];
    let mut lines = vec!["stanza could not fix these in the files you changed:".to_owned()];
    for finding in shown {
        let message = match failure(finding.rule) {
            Some(failure) => format!("{failure}: {}", finding.message),
            None => format!("{} {}", finding.rule.id(), finding.message),
        };

        lines.push(format!(
            "  {}:{}:{} {message}",
            finding.path, finding.line, finding.col
        ));
    }

    if findings.len() > shown.len() {
        lines.push(format!("  ... and {} more", findings.len() - shown.len()));
    }

    let rules: Vec<String> = RULES
        .iter()
        .filter(|entry| findings.iter().any(|finding| finding.rule == entry.rule))
        .map(|entry| format!("{}: {}", entry.rule.id(), entry.summary))
        .collect();

    let mut sections = vec![lines.join("\n")];
    if !rules.is_empty() {
        sections.push(rules.join("\n"));
    }

    sections.push("Fix those findings, then reply again.".to_owned());

    let paths: HashSet<&str> = findings
        .iter()
        .map(|finding| finding.path.as_str())
        .collect();
    let reread: Vec<String> = rewritten
        .iter()
        .filter(|path| paths.contains(path.as_str()))
        .cloned()
        .collect();

    if !reread.is_empty() {
        sections.push(reread_line("rewrote", &reread));
    }

    Some(sections.join("\n\n"))
}

pub fn reread_line(verb: &str, paths: &[String]) -> String {
    let (names, pronoun) = match paths {
        [only] => (only.clone(), "it"),
        [head @ .., last] => (format!("{} and {last}", head.join(", ")), "them"),
        [] => (String::new(), "them"),
    };

    format!("stanza {verb} {names}, so read {pronoun} again before editing.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::stringify;
    use crate::usage::FLAGS;

    fn read(text: &str, cwd: &str) -> String {
        match hook_input(text, cwd) {
            Err(error) => format!("error {error}"),
            Ok(HookInput::Ignored) => "ignored".to_owned(),
            Ok(HookInput::Write { cwd, tool_input }) => {
                let mut echoed = String::new();
                stringify(
                    &tool_input.with_content(tool_input.content.clone()),
                    &mut echoed,
                )
                .unwrap();
                format!("write {cwd} {echoed}")
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
    fn hook_flags_reject_unknown_and_repeated_switches_before_both_braces() {
        let flags = |args: &[&str]| {
            hook_flags(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(
            flags(&["--hunks", "--x"]),
            Err("unexpected argument --x".to_owned())
        );
        assert_eq!(
            flags(&["--braces", "--no-braces", "--braces"]),
            Err("unexpected argument --braces".to_owned())
        );
        assert_eq!(
            flags(&["--braces", "--no-braces"]),
            Err(BOTH_BRACES.to_owned())
        );
        assert_eq!(
            flags(&["--no-braces", "--hunks"]),
            Ok(HookFlags {
                braces: Some(Braces::Off),
                hunks: true
            })
        );
    }

    fn transcript(lines: &[&str]) -> Option<Vec<String>> {
        written_files_in_transcript(&lines.join("\n"))
            .unwrap()
            .map(|written| written.into_iter().collect())
    }

    #[test]
    fn transcripts_coerce_tool_names_and_key_ids_as_javascript_does() {
        let written = transcript(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":["Write"],"input":{"file_path":"/a.ts"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/b.ts"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/c.ts"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":0,"name":"Edit","input":{"file_path":"/d.ts"}},{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/e.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":-0,"is_error":true}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"relative.ts"}}]}}"#,
        ]);

        assert_eq!(written, Some(vec!["/e.ts".to_owned(), "/c.ts".to_owned()]));
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
    fn a_tool_name_with_its_own_to_string_throws() {
        let text = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":{"toString":1}}]}}"#;
        assert!(written_files_in_transcript(text).is_err());
    }

    #[test]
    fn codex_rollouts_walk_changes_in_javascript_key_order() {
        let line = r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","status":"completed","changes":{"/z.ts":{"type":"add"},"/gone.ts":{"type":"delete"},"7":{"type":"update","move_path":"/moved.ts"},"rel.ts":{}}}}}"#;
        let written: Vec<String> = written_files_in_codex_rollout(line)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(written, ["/moved.ts", "/z.ts"]);
        assert_eq!(
            written_files_in_codex_rollout(r#"{"type":"event_msg"}"#),
            None
        );
    }

    fn write_input(content: &str) -> ToolInput {
        let text = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":"a.ts","content":{content}}}}}"#
        );

        match hook_input(&text, "/repo") {
            Ok(HookInput::Write { tool_input, .. }) => tool_input,
            _ => panic!("not a Write: {text}"),
        }
    }

    fn echoed(value: Option<Json>) -> Option<String> {
        let mut out = String::new();
        stringify(&value?, &mut out).unwrap();
        Some(out)
    }

    #[test]
    fn lone_surrogates_are_masked_for_formatting_and_restored_in_order() {
        let tool_input = write_input(r#""a\ud800\u2065b\udc00""#);
        let masked = tool_input.masked();
        assert_eq!(masked.text, "a\u{2065}\u{2065}b\u{2065}");
        assert_eq!(crate::engine::doc::columns("a\u{2065}b", 2), 2);
        assert_eq!(
            echoed(tool_input.updated(&masked, "x\u{2065}\n\u{2065}y\u{2065}".to_owned())),
            Some("{\"file_path\":\"a.ts\",\"content\":\"x\\ud800\\n\u{2065}y\\udc00\"}".to_owned())
        );
        assert_eq!(
            tool_input
                .updated(&masked, "x\u{2065}".to_owned())
                .map(|_| ()),
            None
        );

        let plain = write_input(r#""a\u2065""#);
        let masked = plain.masked();
        assert_eq!(
            echoed(plain.updated(&masked, "b\u{2065}".to_owned())),
            Some("{\"file_path\":\"a.ts\",\"content\":\"b\u{2065}\"}".to_owned())
        );
    }

    fn finding(path: &str, rule: Rule) -> Finding {
        Finding {
            path: path.to_owned(),
            line: 2,
            col: 3,
            rule,
            message: match rule {
                Rule::Parse => "Unexpected token",
                _ => "6 or more statements with no blank line between them; separate the steps",
            }
            .to_owned(),
            fixable: false,
        }
    }

    fn summary(rule: Rule) -> &'static str {
        RULES
            .iter()
            .find(|entry| entry.rule == rule)
            .unwrap()
            .summary
    }

    fn names(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn block_reason_is_absent_without_findings_even_when_files_were_rewritten() {
        assert_eq!(block_reason(&[], &names(&["bodies.ts"])), None);
    }

    #[test]
    fn block_reason_prints_the_exact_finding_summary_and_instruction() {
        assert_eq!(
            block_reason(&[finding("wall.ts", Rule::Wall)], &[]),
            Some(format!(
                "stanza could not fix these in the files you changed:\n  wall.ts:2:3 wall 6 or more statements with no blank line between them; separate the steps\n\nwall: {}\n\nFix those findings, then reply again.",
                summary(Rule::Wall)
            ))
        );
    }

    #[test]
    fn block_reason_caps_finding_lines_at_twelve_and_reports_the_remaining_count() {
        let findings: Vec<Finding> = (0..14)
            .map(|index| finding(&format!("file{index}.ts"), Rule::Wall))
            .collect();

        let reason = block_reason(&findings, &names(&["file12.ts"])).unwrap();
        assert_eq!(
            reason
                .lines()
                .filter(|line| line.starts_with("  file"))
                .count(),
            12
        );
        assert!(reason.contains("  file11.ts:2:3 wall "));
        assert!(!reason.contains("  file12.ts"));
        assert!(reason.ends_with("stanza rewrote file12.ts, so read it again before editing."));
        assert!(reason.contains("\n  ... and 2 more\n\n"));
        assert!(
            !block_reason(&findings[..12], &[])
                .unwrap()
                .contains("... and")
        );
        assert!(
            !block_reason(&findings, &names(&["other.ts"]))
                .unwrap()
                .contains("stanza rewrote")
        );
    }

    #[test]
    fn block_reason_gives_parse_findings_their_own_message_without_a_rule_summary() {
        let parse = Finding {
            line: 1,
            col: 10,
            ..finding("broken.ts", Rule::Parse)
        };

        assert_eq!(
            block_reason(&[parse], &[]).unwrap(),
            "stanza could not fix these in the files you changed:\n  broken.ts:1:10 could not read or parse this file: Unexpected token\n\nFix those findings, then reply again."
        );
    }

    #[test]
    fn block_reason_tells_an_engine_failure_apart_from_a_parse_failure() {
        let failed = Finding {
            rule: Rule::Error,
            message: "RangeError".to_owned(),
            ..finding("deep.ts", Rule::Parse)
        };

        assert!(
            block_reason(&[failed], &[])
                .unwrap()
                .contains("deep.ts:2:3 stanza failed on this file: RangeError")
        );
    }

    #[test]
    fn block_reason_lists_each_rule_once_in_catalog_order() {
        let findings = [
            finding("wall.ts", Rule::Wall),
            finding("steps.ts", Rule::BlockSpacing),
            finding("other.ts", Rule::Wall),
        ];

        let reason = block_reason(&findings, &[]).unwrap();
        assert_eq!(
            reason.split("\n\n").nth(1),
            Some(
                format!(
                    "block-spacing: {}\nwall: {}",
                    summary(Rule::BlockSpacing),
                    summary(Rule::Wall)
                )
                .as_str()
            )
        );
    }

    #[test]
    fn block_reason_names_only_rewritten_files_with_shown_findings() {
        let wall = [finding("wall.ts", Rule::Wall)];
        assert!(
            !block_reason(&wall, &names(&["bodies.ts"]))
                .unwrap()
                .contains("stanza rewrote")
        );
        assert!(
            block_reason(&wall, &names(&["bodies.ts", "wall.ts"]))
                .unwrap()
                .ends_with("\n\nstanza rewrote wall.ts, so read it again before editing.")
        );

        let findings = [
            finding("a.ts", Rule::Wall),
            finding("b.ts", Rule::Wall),
            finding("c.ts", Rule::Wall),
        ];

        assert!(
            block_reason(&findings, &names(&["a.ts", "b.ts"]))
                .unwrap()
                .ends_with("\n\nstanza rewrote a.ts and b.ts, so read them again before editing.")
        );
        assert!(
            block_reason(&findings, &names(&["a.ts", "b.ts", "c.ts", "clean.ts"]))
                .unwrap()
                .ends_with(
                    "\n\nstanza rewrote a.ts, b.ts and c.ts, so read them again before editing."
                )
        );
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

        let input = hook_input(&text, "/repo").expect("valid hook input");
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
