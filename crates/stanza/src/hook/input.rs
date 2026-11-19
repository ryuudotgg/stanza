use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::paths::{is_absolute, resolve};

#[derive(Debug, PartialEq, Eq)]
pub enum Transcript {
    Session(Option<String>),
    Agent(Option<String>),
}

#[derive(Debug)]
pub enum HookInput {
    Ignored,
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

const HOOK_EVENTS: [&str; 3] = ["Stop", "SubagentStop", "PostToolUse"];

const POST_TOOL_USE: &[&str] = &["Write", "Edit", "MultiEdit", "apply_patch"];

#[derive(Deserialize)]
pub(super) struct Object<'a>(#[serde(borrow)] pub(super) IndexMap<String, &'a RawValue>);

impl<'a> Object<'a> {
    pub(super) fn parse(text: &'a str) -> Result<Option<Self>, serde_json::Error> {
        match serde_json::from_str(text) {
            Ok(object) => Ok(Some(object)),
            Err(error) => match serde_json::from_str::<&RawValue>(text) {
                Ok(raw) if !raw.get().starts_with('{') => Ok(None),
                _ => Err(error),
            },
        }
    }

    pub(super) fn field<T: Deserialize<'a>>(
        &self,
        key: &str,
    ) -> Result<Option<T>, serde_json::Error> {
        match self.0.get(key).copied() {
            Some(raw) => serde_json::from_str(raw.get()),
            None => Ok(None),
        }
    }

    pub(super) fn value<T: Deserialize<'a>>(&self, key: &str) -> Option<T> {
        self.field(key).ok().flatten()
    }

    pub(super) fn is(&self, key: &str, expected: &str) -> bool {
        self.value::<String>(key)
            .is_some_and(|text| text == expected)
    }

    fn text(&self, key: &str) -> Result<Option<String>, String> {
        self.field(key).map_err(|_| match self.0.get(key) {
            Some(raw) if raw.get().starts_with('"') => format!("{key} holds an unpaired surrogate"),
            _ => format!("{key} must be a string"),
        })
    }

    fn tool(&self, tools: &[&str]) -> Option<String> {
        self.value::<String>("tool_name")
            .filter(|name| tools.contains(&name.as_str()))
    }
}

fn matches_patch_header(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("*** ")?;
    let (kind, path) = ["Add File", "Delete File", "Update File", "Move to"]
        .into_iter()
        .find_map(|kind| Some((kind, rest.strip_prefix(kind)?.strip_prefix(": ")?)))?;

    (!path.is_empty() && !path.contains('\r')).then_some((kind, path))
}

const PATCH_APPLIED: &str = "Success. Updated the following files:";

fn edited_paths(input: &Object) -> Vec<String> {
    let Some(tool_input) = input.value::<Object>("tool_input") else {
        return Vec::new();
    };

    let Some(tool) = input.tool(POST_TOOL_USE) else {
        return Vec::new();
    };

    if tool != "apply_patch" {
        return tool_input
            .value::<String>("file_path")
            .map(|path| vec![path])
            .unwrap_or_default();
    }

    let Some(command) = tool_input.value::<String>("command") else {
        return Vec::new();
    };

    let applied = input
        .value::<String>("tool_response")
        .is_some_and(|response| response.lines().any(|line| line == PATCH_APPLIED));

    if !applied {
        return Vec::new();
    }

    let mut updating = false;
    let mut paths = Vec::new();
    for line in command.lines() {
        // Codex trims a header line, but only its end inside an Update hunk, where an indented marker is a context line.
        let line = if updating {
            line.trim_end()
        } else {
            line.trim()
        };

        let Some((kind, path)) = matches_patch_header(line) else {
            continue;
        };

        match kind {
            "Move to" if updating => {
                paths.pop();
            }
            "Move to" => continue,
            _ => updating = kind == "Update File",
        }

        if kind != "Delete File" {
            paths.push(path.to_owned());
        }
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
        .is_some_and(|event| !HOOK_EVENTS.contains(&event.as_str()))
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
        Some(cwd) => resolved(&cwd)?,
        None => fallback_cwd.map_err(str::to_owned)?.to_owned(),
    };
    let named = |name: &str| event.as_deref() == Some(name);

    if named("PostToolUse") {
        let paths = edited_paths(&input);
        if paths.is_empty() {
            return Ok(HookInput::Ignored);
        }

        return Ok(HookInput::Edit { cwd: dir, paths });
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

    let path = transcript_path.map(|path| resolved(&path)).transpose()?;
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
        let unreadable = "cannot read the working directory: no such file or directory";
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
        for text in [
            "null",
            "[]",
            r#""input""#,
            "true",
            "1",
            "1e400",
            r#""\ud800""#,
        ] {
            assert_eq!(read(text, "/repo"), "error input must be a JSON object");
        }

        for value in ["42", "1e400", "-1e400"] {
            assert_eq!(
                read(&format!(r#"{{"cwd":{value}}}"#), "/repo"),
                "error cwd must be a string"
            );
        }

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
    fn hook_input_allowlists_stop_subagent_stop_and_post_tool_use() {
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

        for event in ["UserPromptSubmit", "PostToolUseFailure", "PreToolUse"] {
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
            "edit /repo/sub a.ts b.ts sub/../c.ts"
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

        for tool in ["Write", "Edit", "MultiEdit"] {
            let text = post_tool_use(
                tool,
                r#"{"file_path":"src/a.ts","content":"x"}"#,
                Some(r#"{"filePath":"src/a.ts"}"#),
            );
            assert_eq!(read(&text, "/repo"), "edit /repo/sub src/a.ts");
        }
    }

    #[test]
    fn hook_input_reads_patch_lines_as_codex_splits_and_trims_them() {
        let carriage = quoted("Success. Updated the following files:\r\nA a.ts");
        let separated = quoted("x\u{2028}Success. Updated the following files:\u{2028}y");
        let command = format!(
            r#"{{"command":{}}}"#,
            quoted(
                "*** Add File: a.ts\r\n*** Add File: b\r.ts\n*** Add File: c\u{2028}.ts\n*** Add File: \n  *** Add File: d.ts \u{2028}\n*** Add File: e.ts\r"
            )
        );

        assert_eq!(
            read(
                &post_tool_use("apply_patch", &command, Some(&carriage)),
                "/repo"
            ),
            "edit /repo/sub a.ts c\u{2028}.ts d.ts e.ts"
        );

        assert_eq!(
            read(
                &post_tool_use("apply_patch", &command, Some(&separated)),
                "/repo"
            ),
            "ignored"
        );
    }

    #[test]
    fn hook_input_reads_an_indented_marker_inside_an_update_hunk_as_context() {
        let command = format!(
            r#"{{"command":{}}}"#,
            quoted(&patch(&[
                "  *** Update File: a.ts ",
                "*** Move to: y.ts",
                "@@",
                " *** Add File: b.ts",
                "-x",
                "+y",
                "*** Delete File: gone.ts",
                "  *** Add File: c.ts",
                "+c",
                "*** Update File: d.ts\t",
                "*** Move to: e.ts ",
                "@@",
                "  *** Update File: f.ts",
                "-x",
                "+y",
            ]))
        );

        assert_eq!(
            read(
                &post_tool_use("apply_patch", &command, Some(&quoted(APPLIED))),
                "/repo"
            ),
            "edit /repo/sub y.ts c.ts e.ts"
        );
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
            post_tool_use("Write", r#"{"content":"x"}"#, None),
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
                r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_name":"Write","tool_input":{"file_path":"b.ts"},"tool_input":{"file_path":"a.ts","content":"x","file_path":"c.ts"}}"#,
                "/repo"
            ),
            "edit /repo c.ts"
        );
        assert_eq!(
            read(
                &post_tool_use("Edit", r#"{"file_path":42,"file_path":"a.ts"}"#, None),
                "/repo"
            ),
            "edit /repo/sub a.ts"
        );
    }

    #[test]
    fn hook_input_reads_no_path_from_a_lone_surrogate() {
        assert_eq!(
            read(r#"{"\ud800":1,"cwd":"/a"}"#, "/repo"),
            "error input must be valid JSON"
        );
        assert_eq!(
            read(r#"{"cwd":"/a","x":{"\udc00":[]}}"#, "/repo"),
            "stop /a false Session(None)"
        );

        for field in ["cwd", "transcript_path", "hook_event_name"] {
            assert_eq!(
                read(&format!(r#"{{"{field}":"/a\ud800b"}}"#), "/repo"),
                format!("error {field} holds an unpaired surrogate")
            );
        }

        assert_eq!(
            read(
                r#"{"hook_event_name":"SubagentStop","agent_transcript_path":"/a\ud800b"}"#,
                "/repo"
            ),
            "error agent_transcript_path holds an unpaired surrogate"
        );

        let command = format!(
            r#"{{"command":{}}}"#,
            quoted(&patch(&["*** Add File: a.ts", "+x"])).replace("+x", "+\\ud800")
        );
        assert_eq!(
            read(
                &post_tool_use("apply_patch", &command, Some(&quoted(APPLIED))),
                "/repo"
            ),
            "ignored"
        );

        for path in [r#""😀\ud83d.ts""#, r#""\udc00""#, r#""a\ud800\u0041.ts""#] {
            let tool_input = format!(r#"{{"file_path":{path}}}"#);
            assert_eq!(
                read(&post_tool_use("Edit", &tool_input, None), "/repo"),
                "ignored",
                "{path}"
            );
        }

        assert_eq!(
            read(
                &post_tool_use("Edit", r#"{"file_path":"\ud83d\ude00.ts"}"#, None),
                "/repo"
            ),
            "edit /repo/sub \u{1F600}.ts"
        );

        assert_eq!(
            read(
                &post_tool_use("Write", r#"{"file_path":"a.ts","content":"\ud800"}"#, None),
                "/repo"
            ),
            "edit /repo/sub a.ts"
        );
    }

    #[test]
    fn hook_input_ignores_tool_fields_of_another_type() {
        let write = r#"{"file_path":"a.ts","content":"x"}"#;
        for text in [
            post_tool_use("Write", write, None).replace(r#""Write""#, "42"),
            post_tool_use("Write", write, None).replace(r#""Write""#, r#"["Write"]"#),
            post_tool_use("Write", r#""x""#, None),
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
                format!(r#"{{"hook_event_name":"PostToolUse",{write},"transcript_path":42,"stop_hook_active":"x","agent_transcript_path":[]}}"#),
                "edit /repo a.ts",
            ),
            (
                format!(r#"{{"hook_event_name":"PostToolUse",{edit},"transcript_path":{{}},"stop_hook_active":"x"}}"#),
                "edit /repo a.ts",
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
        let candidates = [POST_TOOL_USE, &["NotebookEdit", "Bash", "Read"]].concat();
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
