use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn batch(requests: &[String]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_stanza-dev"))
        .arg("batch")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("stanza-dev starts");

    let mut stdin = child.stdin.take().expect("stdin is piped");
    for request in requests {
        writeln!(stdin, "{request}").expect("the request is written");
    }

    drop(stdin);
    let output = child.wait_with_output().expect("stanza-dev finishes");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");

    String::from_utf8(output.stdout)
        .expect("responses are UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each response is one JSON line"))
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

fn request(id: u64, text: &str, mode: &str, changed_lines: Value, explain: Value) -> Value {
    json!({
        "id": id,
        "path": "probe.ts",
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

fn malformed(response: &Value, id: u64, missing: &str) {
    assert_eq!(response["id"], id);
    assert_eq!(response["text"], Value::Null);
    assert_eq!(response["findings"], json!([]));
    assert_eq!(response["explanations"], json!({}));
    assert!(
        response["error"].as_str().is_some_and(|error| {
            error.starts_with(&format!("malformed request: missing field `{missing}`"))
        }),
        "{response}"
    );
}

#[test]
fn batch_answers_each_request_in_order_and_survives_malformed_ones() {
    let marked = format!("\u{feff}{TEXT}");
    let mut unbraced = request(11, TEXT, "check", Value::Null, json!([]));
    unbraced
        .as_object_mut()
        .expect("a request is an object")
        .remove("braces");

    let responses = batch(&[
        request(7, TEXT, "check", Value::Null, json!([3, 9])).to_string(),
        request(8, &marked, "fix", Value::Null, json!([3])).to_string(),
        "{\"id\": 9, \"mode\": \"fix\"}".to_owned(),
        request(
            10,
            TEXT,
            "fix",
            json!({ "lines": [], "deletedAfter": [] }),
            json!([]),
        )
        .to_string(),
        unbraced.to_string(),
        json!({
            "id": 12,
            "path": "probe.ts",
            "text": BRACED,
            "mode": "check",
            "braces": null,
            "changedLines": null,
            "explain": [1],
        })
        .to_string(),
    ]);

    let finding = json!({
        "path": "probe.ts",
        "line": 3,
        "col": 1,
        "rule": "edge-blank",
        "message": "no blank line right before }",
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

    malformed(&responses[2], 9, "path");
    assert_eq!(responses[3], answered(10, TEXT, json!([]), json!({})));
    malformed(&responses[4], 11, "braces");
    assert_eq!(
        responses[5],
        answered(
            12,
            BRACED,
            json!([{
                "path": "probe.ts",
                "line": 1,
                "col": 8,
                "rule": "braces",
                "message": "braces around a single statement body",
                "fixable": true,
            }]),
            json!({ "1": BRACED_EXPLAINED.join("\n") })
        )
    );
}
