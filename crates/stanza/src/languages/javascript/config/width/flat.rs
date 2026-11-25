use indexmap::IndexMap;

use super::super::data::Data;
use super::object_of;
use crate::text::{is_js_whitespace, js_trim_start, utf16_len};

fn without_comment(text: &str) -> &str {
    let mut at = 0;
    while let Some(offset) = text[at..].find(is_js_whitespace) {
        let start = at + offset;
        let run = &text[start..];
        let after = run.trim_start_matches(is_js_whitespace);
        let hash = start + run.len() - after.len();
        if after.starts_with('#') {
            return &text[..start];
        }

        at = hash;
    }

    text
}

fn scalar(text: &str) -> Data {
    if !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(number) = text.parse()
    {
        return Data::Number(number);
    }

    if text == "true" || text == "false" {
        return Data::Bool(text == "true");
    }

    let quoted = text.len() >= 2
        && (text.starts_with('"') && text.ends_with('"')
            || text.starts_with('\'') && text.ends_with('\''));

    if quoted {
        return Data::String(text[1..text.len() - 1].into());
    }

    Data::String(text.into())
}

fn is_key_char(char: char) -> bool {
    char.is_ascii_alphanumeric() || char == '_' || char == '-'
}

fn toml_key(line: &str) -> Option<(&str, &str)> {
    let end = line.find(|char| !is_key_char(char)).unwrap_or(line.len());
    if end == 0 {
        return None;
    }

    let rest = js_trim_start(&line[end..]).strip_prefix('=')?;
    Some((&line[..end], js_trim_start(rest)))
}

fn misread_comment_meets_lone_cr(text: &str, from: usize) -> bool {
    let mut units = 0;
    let start = text
        .char_indices()
        .find(|&(_, char)| {
            let reached = units >= from;
            units += char.len_utf16();
            reached
        })
        .map_or(text.len(), |(at, _)| at);

    let rest = &text[start..];
    let scanned = match rest.find('\n') {
        Some(end) => rest[..end].strip_suffix('\r').unwrap_or(&rest[..end]),
        None => rest,
    };

    scanned.contains('\r')
}

fn tolerates_trailing_cr(raw: &str) -> bool {
    let value = raw[..raw.find('#').unwrap_or(raw.len())].trim_end();
    value.starts_with(|char: char| char.is_ascii_digit() || char == '+' || char == '-')
        || matches!(value, "true" | "false" | "inf" | "nan")
}

pub(super) fn toml(text: &str) -> Data {
    let mut entries = IndexMap::new();
    for line in text.split_inclusive('\n') {
        let line = line
            .strip_suffix('\n')
            .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line));

        let kept = line.trim_end_matches(is_js_whitespace);
        let trailing_cr = line[kept.len()..].contains('\r');
        if kept.contains('\r') {
            return Data::Unknown;
        }

        let start = js_trim_start(kept);
        if start.is_empty() || start.starts_with('#') {
            if trailing_cr {
                return Data::Unknown;
            }

            continue;
        }

        let Some((key, raw)) = toml_key(kept) else {
            return Data::Unknown;
        };

        if key == "overrides" || raw.is_empty() {
            return Data::Unknown;
        }

        if trailing_cr && !tolerates_trailing_cr(raw) {
            return Data::Unknown;
        }

        // smol-toml scans a bare value's comment from the value's offset into the whole file.
        if !raw.starts_with(['"', '\'', '[', '{'])
            && let Some(hash) = raw.find('#')
            && misread_comment_meets_lone_cr(text, utf16_len(&raw[..hash]))
        {
            return Data::Unknown;
        }

        entries.insert(key.to_owned(), scalar(without_comment(raw)));
    }

    object_of(entries)
}
