use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value as Json;
use stanza::engine::explain::{Request as ExplainRequest, explain};
use stanza::engine::process_file;
use stanza::engine::types::{Braces, FileFindings, Fixed, Mode};
use stanza::files::Changed;
use stanza::languages::javascript::config::width::formatter_width;
use stanza::paths::{basename, dirname, relative, resolve};
use stanza::step::{keep_braces, without_mark};

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

fn answer(request: Request, cwd: &str) -> Response {
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
    let body = without_mark(&request.text);
    let mark = &request.text[..request.text.len() - body.len()];
    let keep = keep_braces(path, braces, &mut Vec::new());
    let result = process_file(
        path,
        body,
        mode,
        keep,
        formatter_width(path).layout(),
        changed,
    );
    let text = match result.fixed {
        Some(Fixed { text, .. }) => format!("{mark}{text}"),
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
            text: body,
            braces: braces.or((!keep).then_some(Braces::On)),
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

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a non-string panic".to_owned());

    format!("internal error: {message}")
}

fn respond(line: &str, cwd: &str) -> Response {
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
    catch_unwind(AssertUnwindSafe(|| answer(request, cwd)))
        .unwrap_or_else(|payload| Response::failed(id, panic_message(payload.as_ref())))
}

pub fn batch(cwd: &str) -> Result<(), String> {
    std::panic::set_hook(Box::new(|_| {}));

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        let response = respond(&line, cwd);
        let printed = serde_json::to_string(&response).expect("responses serialize");
        writeln!(out, "{printed}")
            .and_then(|()| out.flush())
            .map_err(|error| error.to_string())?;
    }

    Ok(())
}
