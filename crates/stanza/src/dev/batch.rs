use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value as Json;
use stanza::engine::explain::{Request as ExplainRequest, explain};
use stanza::engine::types::{Braces, FileFindings, FileResult, Fixed, Mode};
use stanza::files::Changed;
use stanza::format::{catch_panic, silence_caught_panics};
use stanza::paths::{basename, dirname, relative, resolve};
use stanza::step::{Unread, format_text, keep_braces, without_mark};

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ModeName {
    Check,
    Fix,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum BracesName {
    On,
    Off,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ChangedLines {
    lines: BTreeSet<usize>,
    deleted_after: BTreeSet<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Request {
    id: Json,
    path: String,
    text: String,
    mode: ModeName,
    #[serde(deserialize_with = "Option::deserialize")]
    braces: Option<BracesName>,
    #[serde(deserialize_with = "Option::deserialize")]
    changed_lines: Option<ChangedLines>,
    explain: Vec<NonZeroUsize>,
}

#[derive(Serialize)]
struct Response {
    id: Json,
    text: Option<String>,
    #[serde(serialize_with = "each_located")]
    findings: FileFindings,
    explanations: BTreeMap<usize, String>,
    error: Option<String>,
}

fn each_located<S: Serializer>(findings: &FileFindings, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(findings.located())
}

impl Response {
    fn failed(id: Json, error: String) -> Self {
        Response {
            id,
            text: None,
            findings: FileFindings {
                path: String::new(),
                findings: Vec::new(),
            },
            explanations: BTreeMap::new(),
            error: Some(error),
        }
    }
}

type Step = fn(&str, &str, Mode, Option<Braces>, &mut Unread, Option<Changed>) -> FileResult;

fn answer(request: Request, cwd: &str, step: Step) -> Response {
    let braces = request.braces.map(|braces| match braces {
        BracesName::On => Braces::On,
        BracesName::Off => Braces::Off,
    });

    let mode = match request.mode {
        ModeName::Check => Mode::Check,
        ModeName::Fix => Mode::Fix,
    };

    let changed = request.changed_lines.map(|changed| Changed {
        lines: changed.lines,
        deleted_after: changed.deleted_after,
    });

    let path = request.path.as_str();
    let result = step(
        path,
        &request.text,
        mode,
        braces,
        &mut Unread::default(),
        changed,
    );
    let text = match result.fixed {
        Some(Fixed {
            error: Some(error), ..
        }) => {
            return Response::failed(request.id, format!("fixed text does not parse ({error})"));
        }
        Some(Fixed { text, error: None }) => text,
        None => request.text.clone(),
    };

    let mut response = Response {
        id: request.id,
        text: Some(text),
        findings: FileFindings {
            path: request.path.clone(),
            findings: result.findings,
        },
        explanations: BTreeMap::new(),
        error: None,
    };

    if request.explain.is_empty() {
        return response;
    }

    let base = resolve(cwd, &dirname(path));
    let display = |shown: &str| match relative(&base, &resolve(cwd, shown)) {
        shown if shown.is_empty() => basename(path).to_owned(),
        shown => shown,
    };

    let explained = explain(
        &ExplainRequest {
            path,
            text: without_mark(&request.text),
            braces: braces
                .or_else(|| (!keep_braces(path, None, &mut Vec::new())).then_some(Braces::On)),
            display: &display,
        },
        &request.explain,
    );

    match explained {
        Ok(lines) => {
            for (line, explained) in request.explain.iter().zip(lines) {
                let text = match explained {
                    Ok(explained) => explained.lines.join("\n"),
                    Err(error) => error,
                };

                response.explanations.insert(line.get(), text);
            }
        }
        Err(error) => response.error = Some(error),
    }

    response
}

fn respond(line: &str, cwd: &str, step: Step) -> Response {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            let id = serde_json::from_str::<Json>(line)
                .ok()
                .and_then(|value| value.get("id").cloned())
                .unwrap_or(Json::Null);

            return Response::failed(id, format!("malformed request: {error}"));
        }
    };

    let id = request.id.clone();
    catch_panic(|| answer(request, cwd, step))
        .unwrap_or_else(|panic| Response::failed(id, format!("internal error: {panic}")))
}

pub fn batch(cwd: &str) -> Result<(), String> {
    silence_caught_panics();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        let response = respond(&line, cwd, format_text);
        let printed = serde_json::to_string(&response).expect("responses serialize");
        writeln!(out, "{printed}")
            .and_then(|()| out.flush())
            .map_err(|error| error.to_string())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("stanza-dev-batch-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn cwd(&self) -> &str {
            self.0.to_str().unwrap()
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn answers(scratch: &Scratch, requests: &[Value], step: Step) -> Vec<Value> {
        requests
            .iter()
            .map(|request| {
                let response = respond(&request.to_string(), scratch.cwd(), step);
                serde_json::to_value(response).unwrap()
            })
            .collect()
    }

    const BRACED: &str = "if (a) {\n  run();\n}\n";

    const BRACED_EXPLAINED: [&str; 6] = [
        "probe.ts:1",
        "",
        "braced body, lines 1 to 3",
        "  2        run();",
        "rule:      braces, a control flow body that is a single statement has no braces",
        "result:    --fix removes the braces",
    ];

    const TEXT: &str = "function f() {\n  run();\n\n}\n";

    fn request(
        id: u64,
        path: &str,
        text: &str,
        mode: &str,
        changed_lines: Value,
        explain: Value,
    ) -> Value {
        json!({
            "id": id,
            "path": path,
            "text": text,
            "mode": mode,
            "braces": "on",
            "changedLines": changed_lines,
            "explain": explain,
        })
    }

    fn answered(id: u64, text: &str, findings: Value, explanations: Value) -> Value {
        json!({
            "id": id,
            "text": text,
            "findings": findings,
            "explanations": explanations,
            "error": null,
        })
    }

    fn assert_failed(response: &Value, id: u64, error: &str) {
        assert_eq!(response["id"], id);
        assert_eq!(response["text"], Value::Null);
        assert_eq!(response["findings"], json!([]));
        assert_eq!(response["explanations"], json!({}));
        assert!(
            response["error"]
                .as_str()
                .is_some_and(|error_text| error_text.starts_with(error)),
            "{response}"
        );
    }

    #[test]
    fn batch_answers_each_request_in_order_and_survives_malformed_ones() {
        let scratch = Scratch::new("order");
        let path = scratch.path("probe.ts");
        let marked = format!("\u{feff}{TEXT}");
        let mut unbraced = request(11, &path, TEXT, "check", Value::Null, json!([]));
        unbraced.as_object_mut().unwrap().remove("braces");

        let responses = answers(
            &scratch,
            &[
                request(7, &path, TEXT, "check", Value::Null, json!([3, 9])),
                request(8, &path, &marked, "fix", Value::Null, json!([3])),
                json!({ "id": 9, "mode": "fix" }),
                request(
                    10,
                    &path,
                    TEXT,
                    "fix",
                    json!({ "lines": [], "deletedAfter": [] }),
                    json!([]),
                ),
                unbraced,
                json!({
                    "id": 12,
                    "path": path,
                    "text": BRACED,
                    "mode": "check",
                    "braces": null,
                    "changedLines": null,
                    "explain": [1],
                }),
            ],
            format_text,
        );

        let finding = json!({
            "path": path,
            "line": 3,
            "col": 1,
            "rule": "edge-blank",
            "message": "remove the blank line right before `}`",
            "fixable": true,
        });

        let unexplained = "probe.ts:3: no gap ends and no braced body starts on this line";
        let explanations = json!({ "3": unexplained, "9": "probe.ts has 4 lines, not 9" });

        assert_eq!(responses.len(), 6);
        assert_eq!(
            responses[0],
            answered(7, TEXT, json!([finding]), explanations)
        );
        assert_eq!(
            responses[1],
            answered(
                8,
                "\u{feff}function f() {\n  run();\n}\n",
                json!([]),
                json!({ "3": unexplained })
            )
        );

        assert_failed(&responses[2], 9, "malformed request: missing field `path`");
        assert_eq!(responses[3], answered(10, TEXT, json!([]), json!({})));
        assert_failed(
            &responses[4],
            11,
            "malformed request: missing field `braces`",
        );
        assert_eq!(
            responses[5],
            answered(
                12,
                BRACED,
                json!([{
                    "path": path,
                    "line": 1,
                    "col": 8,
                    "rule": "braces",
                    "message": "remove the braces around this single statement body",
                    "fixable": true,
                }]),
                json!({ "1": BRACED_EXPLAINED.join("\n") })
            )
        );
    }

    #[test]
    fn a_fix_that_does_not_parse_answers_no_text() {
        let scratch = Scratch::new("unparsable");
        let path = scratch.path("probe.ts");
        let responses = answers(
            &scratch,
            &[request(1, &path, TEXT, "fix", Value::Null, json!([3]))],
            |_, _, _, _, _, _| FileResult {
                findings: Vec::new(),
                parse_error: false,
                fixed: Some(Fixed {
                    text: "let x = ;".to_owned(),
                    error: Some("unexpected token".to_owned()),
                }),
            },
        );

        assert_failed(
            &responses[0],
            1,
            "fixed text does not parse (unexpected token)",
        );
    }

    #[test]
    fn a_panicking_request_answers_its_id_and_the_next_still_formats() {
        let scratch = Scratch::new("panic");
        let path = scratch.path("probe.ts");
        let panics = scratch.path("panics.ts");
        let responses = answers(
            &scratch,
            &[
                request(1, &panics, TEXT, "fix", Value::Null, json!([])),
                request(2, &path, TEXT, "fix", Value::Null, json!([])),
            ],
            |path, text, mode, braces, unread, lines| {
                assert!(!path.ends_with("panics.ts"), "injected panic");
                format_text(path, text, mode, braces, unread, lines)
            },
        );

        assert_failed(&responses[0], 1, "internal error: injected panic");
        assert_eq!(
            responses[1],
            answered(2, "function f() {\n  run();\n}\n", json!([]), json!({}))
        );
    }
}
