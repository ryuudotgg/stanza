#![no_main]

use libfuzzer_sys::fuzz_target;
use stanza::hook::{hook_input, recorded_writes_in};

fuzz_target!(|bytes: &[u8]| {
    let first = recorded_writes_in(bytes);
    let second = recorded_writes_in(bytes);
    assert_eq!(
        first.as_ref().map(|written| written.as_slice()),
        second.as_ref().map(|written| written.as_slice()),
        "two reads of the same bytes disagree"
    );

    for path in first.iter().flatten() {
        assert!(
            path.starts_with('/'),
            "a reader returned the relative path {path:?}"
        );
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        let _ = hook_input(text, Ok("/fuzz"));
    }
});
