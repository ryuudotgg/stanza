#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Number, Value};
use stanza::hook::hook_input;
use stanza::json::{parse, stringify};

const STACK: usize = 1 << 30;

fn value(text: &str) -> Option<Value> {
    let mut reader = serde_json::Deserializer::from_str(text);
    reader.disable_recursion_limit();

    let mut value = Value::deserialize(&mut reader).ok()?;
    reader.end().ok()?;
    as_floats(&mut value);
    Some(value)
}

fn as_floats(value: &mut Value) {
    match value {
        Value::Number(number) => {
            let float = number.as_f64().expect("serde_json numbers are finite");
            *number = Number::from_f64(float).expect("a finite float");
        }
        Value::Array(items) => items.iter_mut().for_each(as_floats),
        Value::Object(entries) => entries.values_mut().for_each(as_floats),
        _ => {}
    }
}

// IgnoredAny checks syntax only, so like the hook it passes lone surrogates and numbers past f64.
fn check(text: &str) {
    let ours = parse(text);
    let reference = serde_json::from_str::<IgnoredAny>(text).is_ok();
    assert_eq!(
        ours.is_some(),
        reference,
        "stanza and serde_json disagree on accepting it"
    );

    if let (Some(ours), Some(expected)) = (ours, value(text)) {
        let mut written = String::new();
        if stringify(&ours, &mut written).is_ok() {
            assert_eq!(
                value(&written),
                Some(expected),
                "stanza decoded it differently"
            );
        }
    }

    let _ = hook_input(text, Ok("/fuzz"));
}

// serde_json's Value parse, compare and drop all recurse once per nesting level.
fuzz_target!(|bytes: &[u8]| {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };

    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(scope, || check(text))
            .expect("the checking thread starts")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
});
