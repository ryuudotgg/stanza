#![no_main]

use std::fmt;

use libfuzzer_sys::fuzz_target;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use stanza::hook::{HookInput, hook_input};

struct Key(Vec<u8>);

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(KeyVisitor)
    }
}

struct KeyVisitor;

impl Visitor<'_> for KeyVisitor {
    type Value = Key;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a key")
    }

    fn visit_bytes<E>(self, bytes: &[u8]) -> Result<Key, E> {
        Ok(Key(bytes.to_vec()))
    }

    fn visit_str<E>(self, text: &str) -> Result<Key, E> {
        Ok(Key(text.as_bytes().to_vec()))
    }
}

struct Entries<'a>(Vec<(Vec<u8>, &'a RawValue)>);

impl<'de: 'a, 'a> Deserialize<'de> for Entries<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(EntriesVisitor(std::marker::PhantomData))
    }
}

struct EntriesVisitor<'a>(std::marker::PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for EntriesVisitor<'a> {
    type Value = Entries<'a>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an object")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Entries<'a>, M::Error> {
        let mut entries = Vec::new();
        while let Some((Key(key), value)) = map.next_entry::<Key, &'a RawValue>()? {
            entries.push((key, value));
        }

        Ok(Entries(entries))
    }
}

fn last_content(sent: &str) -> &str {
    let parsed: Entries = serde_json::from_str(sent).expect("an object");
    let (_, value) = parsed
        .0
        .into_iter()
        .rfind(|(key, _)| key == b"content")
        .expect("a content entry");

    value.get()
}

fuzz_target!(|bytes: &[u8]| {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };

    let Ok(HookInput::Write { tool_input, .. }) = hook_input(text, Ok("/fuzz")) else {
        return;
    };

    let sent = tool_input.sent();
    let span = tool_input.content_at();
    let last = last_content(sent);
    assert!(
        std::ptr::eq(last, &sent[span.clone()]),
        "the span is not the last content value"
    );

    let decoded: String = serde_json::from_str(last).expect("the content is a JSON string");
    assert_eq!(decoded, tool_input.content, "the content read differs");

    let echo = tool_input
        .echo(&tool_input.content)
        .expect("the echo is JSON");
    let quoted = serde_json::to_string(&tool_input.content).unwrap();
    let expected = [&sent[..span.start], &quoted, &sent[span.end..]].concat();
    assert_eq!(
        echo.get(),
        expected,
        "the echo differs from the tool input outside its last content"
    );
});
