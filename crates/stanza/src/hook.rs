use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use serde::Deserialize;
use serde::de::{Deserializer, Visitor};
use serde_json::value::RawValue;

use crate::engine::rules::{RULES, Rule};
use crate::engine::types::{Braces, Finding};
use crate::files::realpath;
use crate::json::{JsObject, JsString, Json, parse};
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
    fn read(text: &str) -> Option<ToolInput> {
        let Some(Json::Object(object)) = parse(text) else {
            return None;
        };

        let file_path = object.text("file_path")?.clone();
        let content = object.text("content")?.clone();
        Some(ToolInput {
            object: Rc::unwrap_or_clone(object),
            file_path,
            content,
        })
    }

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

#[derive(Debug, PartialEq, Eq, Hash)]
struct Text(Vec<u8>);

impl Text {
    fn is(&self, text: &str) -> bool {
        self.0 == text.as_bytes()
    }

    fn is_any(&self, texts: &[&str]) -> bool {
        texts.iter().any(|text| self.is(text))
    }

    // serde_json gives each lone surrogate escape as 3 WTF-8 bytes.
    fn lossy(&self) -> String {
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
struct Object<'a>(#[serde(borrow)] IndexMap<Text, &'a RawValue>);

impl<'a> Object<'a> {
    // Keys read as bytes skip serde_json's control character check; RawValue runs it.
    fn parse(text: &'a str) -> Result<Option<Self>, serde_json::Error> {
        let raw: &RawValue = serde_json::from_str(text)?;
        Ok(serde_json::from_str(raw.get()).ok())
    }

    fn field<T: Deserialize<'a>>(&self, key: &str) -> Result<Option<T>, serde_json::Error> {
        match self.0.get(key.as_bytes()).copied() {
            Some(raw) => serde_json::from_str(raw.get()),
            None => Ok(None),
        }
    }

    fn value<T: Deserialize<'a>>(&self, key: &str) -> Option<T> {
        self.field(key).ok().flatten()
    }

    fn is(&self, key: &str, expected: &str) -> bool {
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

fn is_candidate_file(path: &str) -> bool {
    realpath(path).is_ok_and(|real| is_candidate(basename(&real), false))
}

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

pub fn hook_call(input: HookInput) -> Option<HookCall> {
    let (cwd, stop_hook_active, transcript) = match input {
        HookInput::Ignored => return None,
        HookInput::Write { cwd, tool_input } => {
            let candidate = is_candidate(basename(&tool_input.path()), false);
            return candidate.then_some(HookCall::Write { cwd, tool_input });
        }
        HookInput::Edit { cwd, paths } => {
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

fn basename_without(path: &str, extension: &str) -> String {
    let name = basename(path);
    match name.strip_suffix(extension) {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => name.to_owned(),
    }
}

pub fn written_files(transcript_path: &str) -> Option<IndexSet<String>> {
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
enum Format {
    Codex,
    Claude,
}

fn recorded_writes(path: &str, codex: bool) -> Option<(Format, IndexSet<String>)> {
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

#[derive(Default)]
struct CodexRollout {
    written: IndexSet<String>,
    recognized: bool,
}

impl CodexRollout {
    fn read(&mut self, line: &str) {
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

    fn written(self) -> Option<IndexSet<String>> {
        self.recognized.then_some(self.written)
    }
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
        match hook_input(text, Ok(cwd)) {
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
            r#"write /repo {"file_path":"c.ts","content":"x"}"#
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

    fn written_files_in_transcript(text: &str) -> Option<IndexSet<String>> {
        let mut transcript = ClaudeTranscript::default();
        text.split('\n').for_each(|line| transcript.read(line));
        transcript.written()
    }

    fn written_files_in_codex_rollout(text: &str) -> Option<IndexSet<String>> {
        let mut rollout = CodexRollout::default();
        text.split('\n').for_each(|line| rollout.read(line));
        rollout.written()
    }

    fn transcript(lines: &[&str]) -> Option<Vec<String>> {
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

    fn write_input(content: &str) -> ToolInput {
        let text = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":"a.ts","content":{content}}}}}"#
        );

        match hook_input(&text, Ok("/repo")) {
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
