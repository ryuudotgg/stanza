use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::text::js_number_string;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum JsString {
    Text(Box<str>),
    Units(Box<[u16]>),
}

impl JsString {
    pub fn lossy(&self) -> Cow<'_, str> {
        match self {
            JsString::Text(text) => Cow::Borrowed(text),
            JsString::Units(units) => Cow::Owned(String::from_utf16_lossy(units)),
        }
    }

    pub fn is(&self, text: &str) -> bool {
        matches!(self, JsString::Text(own) if &**own == text)
    }

    fn units(&self) -> Cow<'_, [u16]> {
        match self {
            JsString::Text(text) => Cow::Owned(text.encode_utf16().collect()),
            JsString::Units(units) => Cow::Borrowed(units),
        }
    }
}

impl From<&str> for JsString {
    fn from(text: &str) -> Self {
        JsString::Text(text.into())
    }
}

#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Array(Rc<JsArray>),
    Object(Rc<JsObject>),
}

#[derive(Clone, Debug, Default)]
pub struct JsArray(Vec<Json>);

impl Deref for JsArray {
    type Target = [Json];

    fn deref(&self) -> &[Json] {
        &self.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct JsObject {
    indexed: BTreeMap<u32, Json>,
    named: IndexMap<JsString, Json>,
}

fn dismantle(mut pending: Vec<Json>) {
    while let Some(value) = pending.pop() {
        match value {
            Json::Array(items) => {
                if let Ok(mut items) = Rc::try_unwrap(items) {
                    pending.append(&mut items.0);
                }
            }
            Json::Object(object) => {
                if let Ok(mut object) = Rc::try_unwrap(object) {
                    pending.extend(object.take_values());
                }
            }
            _ => {}
        }
    }
}

impl Drop for JsArray {
    fn drop(&mut self) {
        dismantle(std::mem::take(&mut self.0));
    }
}

impl Drop for JsObject {
    fn drop(&mut self) {
        dismantle(self.take_values());
    }
}

pub enum JsKey<'a> {
    Index(u32),
    Name(&'a JsString),
}

impl JsKey<'_> {
    pub fn lossy(&self) -> Cow<'_, str> {
        match self {
            JsKey::Index(index) => Cow::Owned(index.to_string()),
            JsKey::Name(name) => name.lossy(),
        }
    }
}

pub fn array_index(key: &str) -> Option<u32> {
    let index: u32 = key.parse().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

impl JsObject {
    fn take_values(&mut self) -> Vec<Json> {
        let indexed = std::mem::take(&mut self.indexed).into_values();
        let named = std::mem::take(&mut self.named).into_values();
        indexed.chain(named).collect()
    }

    pub fn insert(&mut self, key: JsString, value: Json) {
        match &key {
            JsString::Text(text) => match array_index(text) {
                Some(index) => {
                    self.indexed.insert(index, value);
                }
                None => {
                    self.named.insert(key, value);
                }
            },
            JsString::Units(_) => {
                self.named.insert(key, value);
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match array_index(key) {
            Some(index) => self.indexed.get(&index),
            None => self.named.get(&JsString::from(key)),
        }
    }

    pub fn text(&self, key: &str) -> Option<&JsString> {
        match self.get(key)? {
            Json::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn record(&self, key: &str) -> Option<&JsObject> {
        match self.get(key)? {
            Json::Object(object) => Some(object),
            _ => None,
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = (JsKey<'_>, &Json)> {
        let indexed = self
            .indexed
            .iter()
            .map(|(index, value)| (JsKey::Index(*index), value));

        indexed.chain(
            self.named
                .iter()
                .map(|(name, value)| (JsKey::Name(name), value)),
        )
    }
}

pub const CALL_STACK_EXCEEDED: &str = "RangeError: Maximum call stack size exceeded.";

const STRING_DEPTH: usize = 4313;

const STRINGIFY_DEPTH: usize = 40000;

pub fn js_string(value: Option<&Json>) -> Result<Cow<'_, str>, &'static str> {
    coerce(value, 0)
}

fn coerce(value: Option<&Json>, depth: usize) -> Result<Cow<'_, str>, &'static str> {
    let Some(value) = value else {
        return Ok(Cow::Borrowed("undefined"));
    };

    Ok(match value {
        Json::Null => Cow::Borrowed("null"),
        Json::Bool(flag) => Cow::Borrowed(if *flag { "true" } else { "false" }),
        Json::Number(number) => Cow::Owned(js_number_string(*number)),
        Json::String(text) => text.lossy(),
        Json::Array(_) if depth >= STRING_DEPTH => return Err(CALL_STACK_EXCEEDED),
        Json::Array(items) => {
            let mut joined = Vec::with_capacity(items.len());
            for item in items.iter() {
                joined.push(match item {
                    Json::Null => Cow::Borrowed(""),
                    item => coerce(Some(item), depth + 1)?,
                });
            }

            Cow::Owned(joined.join(","))
        }
        Json::Object(object) if object.get("toString").is_some() => {
            return Err("TypeError: No default value");
        }
        Json::Object(_) => Cow::Borrowed("[object Object]"),
    })
}

#[derive(Clone, Debug)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(JsString),
    Array(Rc<JsArray>),
    Object(Rc<JsObject>),
}

impl MapKey {
    pub fn of(value: Option<&Json>) -> Self {
        match value {
            None => MapKey::Undefined,
            Some(Json::Null) => MapKey::Null,
            Some(Json::Bool(flag)) => MapKey::Bool(*flag),
            Some(Json::Number(number)) => MapKey::Number((number + 0.0).to_bits()),
            Some(Json::String(text)) => MapKey::String(text.clone()),
            Some(Json::Array(items)) => MapKey::Array(items.clone()),
            Some(Json::Object(object)) => MapKey::Object(object.clone()),
        }
    }
}

impl PartialEq for MapKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (MapKey::Undefined, MapKey::Undefined) | (MapKey::Null, MapKey::Null) => true,
            (MapKey::Bool(left), MapKey::Bool(right)) => left == right,
            (MapKey::Number(left), MapKey::Number(right)) => left == right,
            (MapKey::String(left), MapKey::String(right)) => left == right,
            (MapKey::Array(left), MapKey::Array(right)) => Rc::ptr_eq(left, right),
            (MapKey::Object(left), MapKey::Object(right)) => Rc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl Eq for MapKey {}

impl Hash for MapKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            MapKey::Undefined | MapKey::Null => {}
            MapKey::Bool(flag) => flag.hash(state),
            MapKey::Number(bits) => bits.hash(state),
            MapKey::String(text) => text.hash(state),
            MapKey::Array(items) => Rc::as_ptr(items).hash(state),
            MapKey::Object(object) => Rc::as_ptr(object).hash(state),
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

enum Built {
    Text(String),
    Units(Vec<u16>),
}

impl Built {
    fn push_str(&mut self, text: &str) {
        match self {
            Built::Text(own) => own.push_str(text),
            Built::Units(units) => units.extend(text.encode_utf16()),
        }
    }

    fn push_unit(&mut self, unit: u16) {
        if let (Built::Text(own), Some(char)) = (&mut *self, char::from_u32(u32::from(unit))) {
            own.push(char);
            return;
        }

        if let Built::Text(own) = self {
            *self = Built::Units(own.encode_utf16().collect());
        }

        if let Built::Units(units) = self {
            units.push(unit);
        }
    }

    fn finish(self) -> JsString {
        match self {
            Built::Text(text) => JsString::Text(text.into()),
            Built::Units(units) => JsString::Units(units.into()),
        }
    }
}

impl Parser<'_> {
    fn bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes().get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(is_space) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        let matched = self.peek() == Some(byte);
        self.at += usize::from(matched);
        matched
    }

    fn word(&mut self, word: &str, value: Json) -> Option<Json> {
        self.text[self.at..].starts_with(word).then(|| {
            self.at += word.len();
            value
        })
    }

    fn scalar(&mut self) -> Option<Json> {
        match self.peek()? {
            b'"' => self.string().map(Json::String),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'n' => self.word("null", Json::Null),
            _ => self.number(),
        }
    }

    fn key(&mut self) -> Option<JsString> {
        self.skip_space();
        if self.peek() != Some(b'"') {
            return None;
        }

        let key = self.string()?;
        self.skip_space();
        self.eat(b':').then_some(key)
    }

    fn value(&mut self) -> Option<Json> {
        let mut open: Vec<Frame> = Vec::new();
        loop {
            self.skip_space();
            let mut value = match self.peek()? {
                b'[' => {
                    self.at += 1;
                    self.skip_space();
                    if !self.eat(b']') {
                        open.push(Frame::Array(Vec::new()));
                        continue;
                    }

                    Json::Array(Rc::default())
                }
                b'{' => {
                    self.at += 1;
                    self.skip_space();
                    if !self.eat(b'}') {
                        let key = self.key()?;
                        open.push(Frame::Object(JsObject::default(), key));
                        continue;
                    }

                    Json::Object(Rc::default())
                }
                _ => self.scalar()?,
            };

            loop {
                let Some(frame) = open.last_mut() else {
                    return Some(value);
                };

                self.skip_space();
                let closed = match frame {
                    Frame::Array(items) => {
                        items.push(value);
                        if self.eat(b',') {
                            break;
                        }

                        self.eat(b']')
                    }
                    Frame::Object(object, key) => {
                        let name = std::mem::replace(key, JsString::from(""));
                        object.insert(name, value);
                        if self.eat(b',') {
                            *key = self.key()?;
                            break;
                        }

                        self.eat(b'}')
                    }
                };

                if !closed {
                    return None;
                }

                value = match open.pop()? {
                    Frame::Array(items) => Json::Array(Rc::new(JsArray(items))),
                    Frame::Object(object, _) => Json::Object(Rc::new(object)),
                };
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.at += 1;
        }

        self.at - start
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        self.eat(b'-');
        if !self.eat(b'0') && self.digits() == 0 {
            return None;
        }

        if self.eat(b'.') && self.digits() == 0 {
            return None;
        }

        if self.eat(b'e') || self.eat(b'E') {
            let _ = self.eat(b'+') || self.eat(b'-');
            if self.digits() == 0 {
                return None;
            }
        }

        self.text[start..self.at].parse().ok().map(Json::Number)
    }

    fn hex(&mut self) -> Option<u16> {
        let digits = self.text.get(self.at..self.at + 4)?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }

        self.at += 4;
        u16::from_str_radix(digits, 16).ok()
    }

    fn escaped_low(&mut self) -> Option<u16> {
        let rest = &self.bytes()[self.at..];
        if !rest.starts_with(b"\\u") {
            return None;
        }

        let save = self.at;
        self.at += 2;
        match self.hex() {
            Some(unit) if (0xdc00..=0xdfff).contains(&unit) => Some(unit),
            _ => {
                self.at = save;
                None
            }
        }
    }

    fn string(&mut self) -> Option<JsString> {
        self.at += 1;
        let mut built = Built::Text(String::new());
        loop {
            let run = self.bytes()[self.at..]
                .iter()
                .position(|&byte| byte == b'"' || byte == b'\\' || byte < 0x20)?;

            built.push_str(&self.text[self.at..self.at + run]);
            self.at += run;
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Some(built.finish());
                }
                b'\\' => self.at += 1,
                _ => return None,
            }

            let escape = self.peek()?;
            self.at += 1;
            let unit = match escape {
                b'"' => 0x22,
                b'\\' => 0x5c,
                b'/' => 0x2f,
                b'b' => 0x08,
                b'f' => 0x0c,
                b'n' => 0x0a,
                b'r' => 0x0d,
                b't' => 0x09,
                b'u' => self.hex()?,
                _ => return None,
            };

            let low = (0xd800..=0xdbff)
                .contains(&unit)
                .then(|| self.escaped_low())
                .flatten();

            match low {
                Some(low) => {
                    let pair = [unit, low];
                    let char = char::decode_utf16(pair).next()?.ok()?;
                    built.push_str(char.encode_utf8(&mut [0; 4]));
                }
                None => built.push_unit(unit),
            }
        }
    }
}

enum Frame {
    Array(Vec<Json>),
    Object(JsObject, JsString),
}

pub fn parse(text: &str) -> Option<Json> {
    let mut parser = Parser { text, at: 0 };
    let value = parser.value()?;
    parser.skip_space();
    (parser.at == text.len()).then_some(value)
}

fn escape_unit(unit: u16, out: &mut String) {
    let _ = write!(out, "\\u{unit:04x}");
}

fn quote_char(char: char, out: &mut String) {
    match char {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\u{8}' => out.push_str("\\b"),
        '\u{c}' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        char if u32::from(char) < 0x20 => escape_unit(char as u16, out),
        char => out.push(char),
    }
}

pub fn quote(text: &JsString, out: &mut String) {
    out.push('"');
    match text {
        JsString::Text(text) => text.chars().for_each(|char| quote_char(char, out)),
        JsString::Units(units) => {
            for decoded in char::decode_utf16(units.iter().copied()) {
                match decoded {
                    Ok(char) => quote_char(char, out),
                    Err(error) => escape_unit(error.unpaired_surrogate(), out),
                }
            }
        }
    }

    out.push('"');
}

pub fn stringify(value: &Json, out: &mut String) -> Result<(), &'static str> {
    write_value(value, out, 0)
}

fn write_value(value: &Json, out: &mut String, depth: usize) -> Result<(), &'static str> {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Number(number) if number.is_finite() => out.push_str(&js_number_string(*number)),
        Json::Number(_) => out.push_str("null"),
        Json::String(text) => quote(text, out),
        Json::Array(_) | Json::Object(_) if depth >= STRINGIFY_DEPTH => {
            return Err(CALL_STACK_EXCEEDED);
        }
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }

                write_value(item, out, depth + 1)?;
            }

            out.push(']');
        }
        Json::Object(object) => {
            out.push('{');
            for (index, (key, item)) in object.entries().enumerate() {
                if index > 0 {
                    out.push(',');
                }

                match key {
                    JsKey::Index(position) => {
                        let _ = write!(out, "\"{position}\"");
                    }
                    JsKey::Name(name) => quote(name, out),
                }

                out.push(':');
                write_value(item, out, depth + 1)?;
            }

            out.push('}');
        }
    }

    Ok(())
}

pub fn concat(parts: &[&JsString]) -> JsString {
    if let Some(texts) = parts
        .iter()
        .map(|part| match part {
            JsString::Text(text) => Some(&**text),
            JsString::Units(_) => None,
        })
        .collect::<Option<Vec<&str>>>()
    {
        return JsString::Text(texts.concat().into());
    }

    let units: Vec<u16> = parts
        .iter()
        .flat_map(|part| part.units().into_owned())
        .collect();
    JsString::Units(units.into())
}

pub fn object(entries: Vec<(&str, Json)>) -> Json {
    let mut built = JsObject::default();
    for (key, value) in entries {
        built.insert(JsString::from(key), value);
    }

    Json::Object(Rc::new(built))
}

pub fn text(value: &str) -> Json {
    Json::String(JsString::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(text: &str) -> String {
        let mut out = String::new();
        stringify(&parse(text).expect("valid JSON"), &mut out).unwrap();
        out
    }

    #[test]
    fn orders_index_keys_first_and_keeps_the_first_position_of_a_duplicate() {
        assert_eq!(
            round(r#"{"b":1,"2":2,"a":3,"1":4,"01":5,"4294967295":6,"b":7}"#),
            r#"{"1":4,"2":2,"b":7,"a":3,"01":5,"4294967295":6}"#
        );
    }

    #[test]
    fn prints_numbers_as_javascript_does() {
        assert_eq!(
            round("[-0,1.0,1e21,1e400,-1e400,9007199254740993,1e-7,0.000001]"),
            "[0,1,1e+21,null,null,9007199254740992,1e-7,0.000001]"
        );
    }

    #[test]
    fn keeps_lone_surrogates_and_joins_escaped_pairs() {
        assert_eq!(
            round(r#"["\ud83d\ude00","\ud800","a\udc00b","\u0000\u001f\u007f\u2028","\/"]"#),
            "[\"\u{1f600}\",\"\\ud800\",\"a\\udc00b\",\"\\u0000\\u001f\u{7f}\u{2028}\",\"/\"]"
        );
    }

    #[test]
    fn rejects_what_json_parse_rejects() {
        for text in [
            "",
            "\u{feff}{}",
            "{,}",
            "[1,]",
            "01",
            "1.",
            ".5",
            "+1",
            "'a'",
            "\"\t\"",
            "\"\\x\"",
            "\"\\u12\"",
            "{\"a\" 1}",
            "nul",
            "1 2",
            "\u{a0}1",
        ] {
            assert!(parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn coerces_values_to_strings_as_string_does() {
        let value = parse(r#"[["Write"],[null,"a"],[1.5,true],{},{"toString":1}]"#).unwrap();
        let Json::Array(items) = value else {
            unreachable!()
        };

        let coerced: Vec<Option<String>> = items
            .iter()
            .map(|item| js_string(Some(item)).ok().map(Cow::into_owned))
            .collect();

        assert_eq!(
            coerced,
            [
                Some("Write".to_owned()),
                Some(",a".to_owned()),
                Some("1.5,true".to_owned()),
                Some("[object Object]".to_owned()),
                None
            ]
        );
        assert_eq!(js_string(None).as_deref(), Ok("undefined"));
    }

    #[test]
    fn map_keys_compare_by_same_value_zero() {
        let value = parse(r#"[0,-0,"0",{},{},null]"#).unwrap();
        let Json::Array(items) = value else {
            unreachable!()
        };

        let keys: Vec<MapKey> = items.iter().map(|item| MapKey::of(Some(item))).collect();
        assert_eq!(keys[0], keys[1]);
        assert_ne!(keys[0], keys[2]);
        assert_ne!(keys[3], keys[4]);
        assert_eq!(keys[3], keys[3].clone());
        assert_ne!(keys[5], MapKey::of(None));
    }

    fn nested(depth: usize) -> String {
        format!("{}\"Write\"{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn parse_and_drop_never_recurse() {
        let deep = format!(
            "{}0{}",
            "[{\"a\":".repeat(1_000_000),
            "}]".repeat(1_000_000)
        );

        assert!(parse(&deep).is_some());
        assert!(parse(&deep[..deep.len() - 1]).is_none());
    }

    #[test]
    fn stringify_and_string_coercion_throw_where_the_reference_does() {
        let limits = std::thread::Builder::new()
            .stack_size(crate::pool::STACK_BYTES)
            .spawn(|| {
                let stringified =
                    |depth| stringify(&parse(&nested(depth)).unwrap(), &mut String::new());
                let coerced =
                    |depth| js_string(parse(&nested(depth)).as_ref()).map(Cow::into_owned);
                (
                    [stringified(40000), stringified(40001)],
                    [coerced(4313), coerced(4314)],
                )
            })
            .unwrap()
            .join()
            .unwrap();

        assert_eq!(
            limits,
            (
                [Ok(()), Err(CALL_STACK_EXCEEDED)],
                [Ok("Write".to_owned()), Err(CALL_STACK_EXCEEDED)]
            )
        );
    }
}
