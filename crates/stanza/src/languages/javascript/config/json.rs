use std::rc::Rc;

use indexmap::IndexMap;

use super::evaluate::Value;
use super::object::Object;

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        while self.bytes.get(self.at).copied().is_some_and(is_space) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        let matched = self.bytes.get(self.at) == Some(&byte);
        self.at += usize::from(matched);
        matched
    }

    fn word(&mut self, word: &str, value: Value) -> Option<Value> {
        self.text[self.at..].starts_with(word).then(|| {
            self.at += word.len();
            value
        })
    }

    fn value(&mut self) -> Option<Value> {
        self.skip_space();
        match self.bytes.get(self.at)? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(|text| Value::String(text.into())),
            b't' => self.word("true", Value::Bool(true)),
            b'f' => self.word("false", Value::Bool(false)),
            b'n' => self.word("null", Value::Null),
            _ => self.number(),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }

        self.at - start
    }

    fn number(&mut self) -> Option<Value> {
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

        self.text[start..self.at].parse().ok().map(Value::Number)
    }

    fn hex(&mut self) -> Option<u16> {
        let digits = self.text.get(self.at..self.at + 4)?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }

        self.at += 4;
        u16::from_str_radix(digits, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        self.at += 1;
        let mut units: Vec<u16> = Vec::new();
        loop {
            let char = self.text[self.at..].chars().next()?;
            self.at += char.len_utf8();
            match char {
                '"' => return Some(String::from_utf16_lossy(&units)),
                '\\' => {
                    let escape = *self.bytes.get(self.at)?;
                    self.at += 1;
                    let unit = match escape {
                        b'"' => u16::from(b'"'),
                        b'\\' => u16::from(b'\\'),
                        b'/' => u16::from(b'/'),
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'n' => u16::from(b'\n'),
                        b'r' => u16::from(b'\r'),
                        b't' => u16::from(b'\t'),
                        b'u' => self.hex()?,
                        _ => return None,
                    };

                    units.push(unit);
                }
                char if u32::from(char) < 0x20 => return None,
                char => {
                    let mut buffer = [0; 2];
                    units.extend_from_slice(char.encode_utf16(&mut buffer));
                }
            }
        }
    }

    fn array(&mut self) -> Option<Value> {
        self.at += 1;
        let mut items = Vec::new();
        self.skip_space();
        if !self.eat(b']') {
            loop {
                items.push(self.value()?);
                self.skip_space();
                if self.eat(b']') {
                    break;
                }

                if !self.eat(b',') {
                    return None;
                }
            }
        }

        Some(Value::Array(Rc::new(items)))
    }

    fn object(&mut self) -> Option<Value> {
        self.at += 1;
        let mut entries: IndexMap<String, Value> = IndexMap::new();
        self.skip_space();
        if !self.eat(b'}') {
            loop {
                self.skip_space();
                if self.bytes.get(self.at) != Some(&b'"') {
                    return None;
                }

                let key = self.string()?;
                self.skip_space();
                if !self.eat(b':') {
                    return None;
                }

                entries.insert(key, self.value()?);
                self.skip_space();
                if self.eat(b'}') {
                    break;
                }

                if !self.eat(b',') {
                    return None;
                }
            }
        }

        Some(Value::Object(Object::plain(
            entries
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect(),
        )))
    }
}

pub fn parse(text: &str) -> Option<Value> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        at: 0,
    };

    let value = parser.value()?;
    parser.skip_space();
    (parser.at == text.len()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(text: &str) -> Option<f64> {
        match parse(text)? {
            Value::Number(value) => Some(value),
            _ => None,
        }
    }

    #[test]
    fn accepts_what_json_parse_accepts() {
        assert_eq!(number("1e400"), Some(f64::INFINITY));
        assert_eq!(number(" -0.5E+2 "), Some(-50.0));
        assert!(matches!(parse(r#""\ud83d""#), Some(Value::String(text)) if &*text == "\u{fffd}"));
        assert!(matches!(parse(r#""😀""#), Some(Value::String(text)) if &*text == "😀"));
        assert!(parse(&format!("{}{}", "[".repeat(500), "]".repeat(500))).is_some());
    }

    #[test]
    fn rejects_what_json_parse_rejects() {
        for text in [
            "",
            "01",
            "1.",
            ".5",
            "+1",
            "[1,]",
            "{\"a\":1,}",
            "'a'",
            "\"\t\"",
            "nul",
            "[] x",
            "\u{feff}{}",
        ] {
            assert!(parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn duplicate_keys_keep_the_first_position_and_the_last_value() {
        let Some(Value::Object(object)) = parse(r#"{"a": 1, "b": 2, "a": 3}"#) else {
            panic!("an object");
        };

        assert_eq!(object.keys(), ["a", "b"]);
    }
}
