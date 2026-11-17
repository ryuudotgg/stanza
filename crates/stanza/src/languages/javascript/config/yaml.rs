use std::fs::File;
use std::io::{self, Read};

use indexmap::IndexMap;
use indexmap::map::Entry;

const MAX_BYTES: usize = 1_048_576;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 100_000;
const MAX_KEY_SPAN: usize = 1024;
const MAX_NUMBER_DIGITS: usize = 20;
const BOM: &str = "\u{feff}";

#[derive(Debug, PartialEq)]
pub enum Tree {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Tree>),
    Object(IndexMap<String, Tree>),
}

pub fn load(file: &str) -> Option<Tree> {
    let source = File::open(file).ok()?;
    parse(&capped(source).ok()?)
}

fn capped(source: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    source.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn parse(bytes: &[u8]) -> Option<Tree> {
    if bytes.len() > MAX_BYTES {
        return None;
    }

    Reader::new(std::str::from_utf8(bytes).ok()?).document()
}

#[derive(Default)]
struct Budget {
    nodes: usize,
    depth: usize,
}

impl Budget {
    fn node(&mut self) -> Option<()> {
        if self.nodes == MAX_NODES {
            return None;
        }

        self.nodes += 1;
        Some(())
    }

    fn descend(&mut self) -> Option<()> {
        if self.depth == MAX_DEPTH {
            return None;
        }

        self.node()?;
        self.depth += 1;
        Some(())
    }
}

enum Line {
    Blank,
    Comment,
    Marker,
    End,
    Content,
}

enum Head {
    Dash,
    Flow,
    Key(String),
    Scalar(Tree),
}

enum Token<'a> {
    Plain(&'a str),
    Quoted(String),
}

enum Plain {
    Null,
    Bool(bool),
    Number(f64),
    Text,
}

enum Decimal {
    Other,
    Core,
    Json,
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
    line: usize,
    column: usize,
    flows: usize,
    budget: Budget,
}

const fn is_blank(byte: Option<u8>) -> bool {
    matches!(byte, Some(b' ' | b'\t'))
}

const fn ends_line(byte: Option<u8>) -> bool {
    matches!(byte, None | Some(b'\n' | b'\r'))
}

const fn separates(byte: Option<u8>) -> bool {
    is_blank(byte) || ends_line(byte)
}

const fn is_flow_indicator(byte: Option<u8>) -> bool {
    matches!(byte, Some(b',' | b'[' | b']' | b'{' | b'}'))
}

const fn starts_plain(first: Option<u8>, second: Option<u8>) -> bool {
    match first {
        Some(b'-' | b'?' | b':') => !(separates(second) || is_flow_indicator(second)),
        Some(
            b',' | b'[' | b']' | b'{' | b'}' | b'#' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\''
            | b'"' | b'%' | b'@' | b'`',
        ) => false,
        first => !separates(first),
    }
}

impl<'a> Reader<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            at: 0,
            line: 0,
            column: 0,
            flows: 0,
            budget: Budget::default(),
        }
    }

    fn byte(&self, ahead: usize) -> Option<u8> {
        self.text.as_bytes().get(self.at + ahead).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let matched = self.byte(0) == Some(byte);
        self.at += usize::from(matched);
        matched
    }

    const fn at_end(&self) -> bool {
        self.at == self.text.len()
    }

    fn blanks(&mut self) -> bool {
        let mut tab = false;
        while is_blank(self.byte(0)) {
            tab |= self.byte(0) == Some(b'\t');
            self.at += 1;
        }

        tab
    }

    fn comment(&mut self) {
        while !ends_line(self.byte(0)) {
            self.at += 1;
        }
    }

    fn line_break(&mut self) -> Option<()> {
        self.at += match (self.byte(0), self.byte(1)) {
            (None, _) => 0,
            (Some(b'\n'), _) => 1,
            (Some(b'\r'), Some(b'\n')) => 2,
            _ => return None,
        };

        Some(())
    }

    fn at_comment(&self) -> bool {
        let before = self.at.checked_sub(1).map(|at| self.text.as_bytes()[at]);
        self.byte(0) == Some(b'#') && is_blank(before)
    }

    fn at_line_end(&self) -> bool {
        ends_line(self.byte(0)) || self.at_comment()
    }

    fn at_dash(&self) -> bool {
        self.byte(0) == Some(b'-') && separates(self.byte(1))
    }

    fn trailing(&mut self) -> Option<()> {
        self.blanks();
        if self.at_comment() {
            self.comment();
        }

        self.line_break()
    }

    fn leaf(&mut self, tree: Tree) -> Option<Tree> {
        self.budget.node()?;
        Some(tree)
    }

    fn finish(&mut self, tree: Tree) -> Option<Tree> {
        self.next_content()?;
        Some(tree)
    }

    fn document(&mut self) -> Option<Tree> {
        let mut marked = false;
        loop {
            match self.line_start()? {
                Line::Blank | Line::Comment => {}
                Line::End => return self.leaf(Tree::Null),
                Line::Content => break,
                Line::Marker if marked => return None,
                Line::Marker => {
                    marked = true;
                    self.trailing()?;
                }
            }
        }

        let column = self.column;
        let root = match self.head()? {
            Head::Dash => self.sequence(column),
            Head::Key(key) => self.map(column, key),
            Head::Scalar(value) => self.finish(value),
            Head::Flow => self.flow(None),
        }?;

        self.at_end().then_some(root)
    }

    fn line_start(&mut self) -> Option<Line> {
        self.line = self.at;
        while self.byte(0) == Some(b' ') {
            self.at += 1;
        }

        self.column = self.at - self.line;
        if self.byte(0) == Some(b'\t') {
            if self.flows == 0 {
                return None;
            }

            self.blanks();
        }

        let rest = &self.text[self.at..];
        let marks = self.column == 0 && separates(self.byte(3));
        match self.byte(0) {
            None => Some(Line::End),
            Some(b'\n' | b'\r') => self.line_break().map(|()| Line::Blank),
            Some(b'#') => {
                self.comment();
                self.line_break().map(|()| Line::Comment)
            }
            _ if self.at == self.line && rest.starts_with(BOM) => None,
            _ if marks && rest.starts_with("...") => None,
            _ if marks && rest.starts_with("---") => {
                self.at += 3;
                Some(Line::Marker)
            }
            _ => Some(Line::Content),
        }
    }

    fn next_content(&mut self) -> Option<()> {
        self.trailing()?;
        self.skip_lines(true)
    }

    fn skip_lines(&mut self, comment_lines: bool) -> Option<()> {
        loop {
            match self.line_start()? {
                Line::Blank => {}
                Line::Comment if comment_lines => {}
                Line::Comment | Line::Marker => return None,
                Line::End | Line::Content => return Some(()),
            }
        }
    }

    fn within(&mut self, body: impl FnOnce(&mut Self) -> Option<Tree>) -> Option<Tree> {
        self.budget.descend()?;

        let tree = body(self)?;
        self.budget.depth -= 1;
        Some(tree)
    }

    fn collection(&mut self) -> Option<Tree> {
        let column = self.column;
        match self.head()? {
            Head::Dash => self.sequence(column),
            Head::Key(key) => self.map(column, key),
            Head::Scalar(_) | Head::Flow => None,
        }
    }

    fn nested(&mut self, owner: usize) -> Option<Tree> {
        if !self.at_end() && self.column > owner {
            self.collection()
        } else {
            self.leaf(Tree::Null)
        }
    }

    fn sequence(&mut self, column: usize) -> Option<Tree> {
        self.within(|reader| {
            let mut items = Vec::new();
            loop {
                let tab = reader.blanks();
                items.push(reader.item(column, tab)?);
                if !(reader.column == column && reader.at_dash()) {
                    break;
                }

                reader.at += 1;
            }

            Some(Tree::Array(items))
        })
    }

    fn item(&mut self, dash: usize, tab: bool) -> Option<Tree> {
        if self.at_line_end() {
            self.next_content()?;
            return self.nested(dash);
        }

        let column = self.at - self.line;
        match self.head()? {
            Head::Scalar(value) => self.finish(value),
            Head::Flow => self.flow(Some(dash)),
            Head::Dash | Head::Key(_) if tab => None,
            Head::Dash => self.sequence(column),
            Head::Key(key) => self.map(column, key),
        }
    }

    fn map(&mut self, column: usize, first: String) -> Option<Tree> {
        self.within(|reader| {
            let mut entries = IndexMap::new();
            let mut key = first;
            loop {
                let Entry::Vacant(slot) = entries.entry(key) else {
                    return None;
                };

                slot.insert(reader.value(column)?);
                if reader.at_end() || reader.column != column {
                    break;
                }

                let Head::Key(next) = reader.head()? else {
                    return None;
                };

                key = next;
            }

            Some(Tree::Object(entries))
        })
    }

    fn value(&mut self, column: usize) -> Option<Tree> {
        self.blanks();
        if !self.at_line_end() {
            return match self.head()? {
                Head::Scalar(value) => self.finish(value),
                Head::Flow => self.flow(Some(column)),
                Head::Dash | Head::Key(_) => None,
            };
        }

        self.next_content()?;
        if self.column == column && self.at_dash() {
            self.at += 1;
            return self.sequence(column);
        }

        self.nested(column)
    }

    fn head(&mut self) -> Option<Head> {
        let start = self.at;
        if self.at_dash() {
            self.at += 1;
            return Some(Head::Dash);
        }

        if matches!(self.byte(0), Some(b'[' | b'{')) {
            return Some(Head::Flow);
        }

        let token = self.token(false)?;
        if !(self.byte(0) == Some(b':') && separates(self.byte(1))) {
            return Some(Head::Scalar(scalar(token)?));
        }

        if self.at - start > MAX_KEY_SPAN {
            return None;
        }

        self.at += 1;
        token.key().map(Head::Key)
    }

    fn token(&mut self, flow: bool) -> Option<Token<'a>> {
        self.budget.node()?;
        if matches!(self.byte(0), Some(b'"' | b'\'')) {
            let text = self.quoted()?;
            self.blanks();
            return Some(Token::Quoted(text));
        }

        self.plain(flow).map(Token::Plain)
    }

    fn plain(&mut self, flow: bool) -> Option<&'a str> {
        if !starts_plain(self.byte(0), self.byte(1)) {
            return None;
        }

        let start = self.at;
        let mut end = self.at;
        loop {
            let (byte, next) = (self.byte(0), self.byte(1));
            if ends_line(byte) || self.at_comment() {
                break;
            }

            if byte == Some(b':') && (separates(next) || (flow && is_flow_indicator(next))) {
                break;
            }

            if flow && is_flow_indicator(byte) {
                break;
            }

            self.at += 1;
            if !is_blank(byte) {
                end = self.at;
            }
        }

        Some(&self.text[start..end])
    }

    fn quoted(&mut self) -> Option<String> {
        let quote = self.byte(0)?;
        self.at += 1;

        let mut text = String::new();
        let mut copied = self.at;
        loop {
            let byte = self
                .byte(0)
                .filter(|&byte| byte != b'\n' && byte != b'\r')?;
            if byte != quote && !(byte == b'\\' && quote == b'"') {
                self.at += 1;
                continue;
            }

            text.push_str(&self.text[copied..self.at]);
            self.at += 1;
            if byte == b'\\' {
                text.push(self.escape()?);
            } else if quote == b'\'' && self.eat(b'\'') {
                text.push('\'');
            } else {
                return Some(text);
            }

            copied = self.at;
        }
    }

    fn escape(&mut self) -> Option<char> {
        let byte = self.byte(0)?;
        self.at += 1;
        match byte {
            b'"' | b'\\' | b'/' => Some(char::from(byte)),
            b'b' => Some('\u{8}'),
            b'f' => Some('\u{c}'),
            b'n' => Some('\n'),
            b'r' => Some('\r'),
            b't' => Some('\t'),
            b'u' => self.unicode(),
            _ => None,
        }
    }

    fn unicode(&mut self) -> Option<char> {
        let unit = self.hex()?;
        if !(0xd800..0xdc00).contains(&unit) {
            return char::from_u32(unit);
        }

        if !(self.eat(b'\\') && self.eat(b'u')) {
            return None;
        }

        let low = self.hex()?;
        if !(0xdc00..0xe000).contains(&low) {
            return None;
        }

        char::from_u32(0x1_0000 + ((unit - 0xd800) << 10) + (low - 0xdc00))
    }

    fn hex(&mut self) -> Option<u32> {
        let digits = self.text.get(self.at..self.at + 4)?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }

        self.at += 4;
        u32::from_str_radix(digits, 16).ok()
    }

    fn flow(&mut self, owner: Option<usize>) -> Option<Tree> {
        let tree = self.flow_value(owner.map_or(0, |column| column + 1))?;
        self.finish(tree)
    }

    fn flow_value(&mut self, floor: usize) -> Option<Tree> {
        match self.byte(0) {
            Some(b'[') => self.flow_sequence(floor),
            Some(b'{') => self.flow_map(floor),
            _ => scalar(self.token(true)?),
        }
    }

    fn gap(&mut self, floor: usize, settled: bool) -> Option<bool> {
        self.blanks();
        if !self.at_line_end() {
            return Some(false);
        }

        self.trailing()?;
        self.skip_lines(!settled)?;

        let closes = self.flows == 1
            && matches!(self.byte(0), Some(b']' | b'}'))
            && self.column + 1 == floor;

        (!self.at_end() && (self.column >= floor || closes)).then_some(true)
    }

    fn flow_sequence(&mut self, floor: usize) -> Option<Tree> {
        self.within(|reader| {
            reader.at += 1;
            reader.flows += 1;

            let mut items = Vec::new();
            loop {
                reader.gap(floor, false)?;
                if reader.eat(b']') {
                    break;
                }

                items.push(reader.flow_value(floor)?);

                let broke = reader.gap(floor, true)?;
                if reader.eat(b']') {
                    break;
                }

                if broke || !reader.eat(b',') {
                    return None;
                }
            }

            reader.flows -= 1;
            Some(Tree::Array(items))
        })
    }

    fn flow_map(&mut self, floor: usize) -> Option<Tree> {
        self.within(|reader| {
            reader.at += 1;
            reader.flows += 1;

            let mut entries = IndexMap::new();
            loop {
                reader.gap(floor, false)?;
                if reader.eat(b'}') {
                    break;
                }

                let Entry::Vacant(slot) = entries.entry(reader.flow_key()?) else {
                    return None;
                };

                reader.blanks();
                let value = if matches!(reader.byte(0), Some(b',' | b'}')) {
                    reader.leaf(Tree::Null)?
                } else {
                    reader.flow_value(floor)?
                };

                slot.insert(value);

                let broke = reader.gap(floor, true)?;
                if reader.eat(b'}') {
                    break;
                }

                if broke || !reader.eat(b',') {
                    return None;
                }
            }

            reader.flows -= 1;
            Some(Tree::Object(entries))
        })
    }

    fn flow_key(&mut self) -> Option<String> {
        let token = self.token(true)?;
        if !self.eat(b':') {
            return None;
        }

        token.key()
    }
}

impl Token<'_> {
    fn key(self) -> Option<String> {
        match self {
            Self::Quoted(text) => Some(text),
            Self::Plain("<<") => None,
            Self::Plain(text) => matches!(classify(text)?, Plain::Text).then(|| text.to_owned()),
        }
    }
}

fn scalar(token: Token<'_>) -> Option<Tree> {
    Some(match token {
        Token::Quoted(text) => Tree::String(text),
        Token::Plain(text) => match classify(text)? {
            Plain::Null => Tree::Null,
            Plain::Bool(value) => Tree::Bool(value),
            Plain::Number(value) => Tree::Number(value),
            Plain::Text => Tree::String(text.to_owned()),
        },
    })
}

fn classify(text: &str) -> Option<Plain> {
    match text {
        "~" | "null" => return Some(Plain::Null),
        "true" => return Some(Plain::Bool(true)),
        "false" => return Some(Plain::Bool(false)),
        "Null" | "NULL" | "True" | "TRUE" | "False" | "FALSE" | ".nan" | ".NaN" | ".NAN" => {
            return None;
        }
        _ => {}
    }

    let signed = text.starts_with(['-', '+']);
    let unsigned = &text[usize::from(signed)..];
    if !unsigned.starts_with(|char: char| char == '.' || char.is_ascii_digit()) {
        return Some(Plain::Text);
    }

    if (!signed && radix(unsigned)) || matches!(unsigned, ".inf" | ".Inf" | ".INF") {
        return None;
    }

    match decimal(unsigned) {
        Decimal::Other => Some(Plain::Text),
        Decimal::Json if !text.starts_with('+') => number(text).map(Plain::Number),
        Decimal::Json | Decimal::Core => None,
    }
}

// ECMAScript lets an engine round a decimal literal past its 20th significant digit, so longer mantissas may not match what yaml read.
fn number(text: &str) -> Option<f64> {
    let mantissa = text.find(['e', 'E']).map_or(text, |at| &text[..at]);
    if mantissa.bytes().filter(u8::is_ascii_digit).count() > MAX_NUMBER_DIGITS {
        return None;
    }

    text.parse().ok().filter(|value: &f64| value.is_finite())
}

fn radix(text: &str) -> bool {
    let all =
        |digits: &str, digit: fn(char) -> bool| !digits.is_empty() && digits.chars().all(digit);

    text.strip_prefix("0x")
        .is_some_and(|digits| all(digits, |char| char.is_ascii_hexdigit()))
        || text
            .strip_prefix("0o")
            .is_some_and(|digits| all(digits, |char| ('0'..='7').contains(&char)))
}

fn decimal(text: &str) -> Decimal {
    let bytes = text.as_bytes();
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };

    let whole = digits(0);
    let fraction = (bytes.get(whole) == Some(&b'.')).then(|| digits(whole + 1));
    let mut at = whole + fraction.map_or(0, |count| count + 1);
    if whole == 0 && fraction.unwrap_or(0) == 0 {
        return Decimal::Other;
    }

    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1 + usize::from(matches!(bytes.get(at + 1), Some(b'+' | b'-')));
        let exponent = digits(at);
        if exponent == 0 {
            return Decimal::Other;
        }

        at += exponent;
    }

    if at != bytes.len() {
        return Decimal::Other;
    }

    if whole > 0 && (whole == 1 || bytes[0] != b'0') && fraction != Some(0) {
        Decimal::Json
    } else {
        Decimal::Core
    }
}

#[cfg(test)]
#[path = "yaml_tests.rs"]
mod tests;
