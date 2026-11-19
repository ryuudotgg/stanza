#![no_main]

use libfuzzer_sys::fuzz_target;
use stanza::hook::hook_input;

fuzz_target!(|bytes: &[u8]| {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };

    let _ = hook_input(text, Ok("/fuzz"));
});
