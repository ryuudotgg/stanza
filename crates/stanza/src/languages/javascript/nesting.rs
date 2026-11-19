use std::cell::Cell;
use std::rc::Rc;

use oxc_span::SourceType;

use crate::pool::thread_stack;

pub const MOST_LEVELS: usize = 10_000;

pub const BYTES_PER_LEVEL: usize = 3072;

const UNITS_PER_LEVEL: usize = 16;

const FALLBACK_STACK: usize = 1 << 19;

const MOST_READINGS: usize = 64;

pub fn cap(stack: usize) -> usize {
    MOST_LEVELS.min(stack / (4 * BYTES_PER_LEVEL))
}

thread_local! {
    static THREAD_CAP: Cell<Option<usize>> = const { Cell::new(None) };
}

pub fn thread_cap() -> usize {
    THREAD_CAP.with(|known| {
        let found = known
            .get()
            .unwrap_or_else(|| cap(thread_stack().unwrap_or(FALLBACK_STACK)));
        known.set(Some(found));
        found
    })
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Brace {
    Block,
    Guessed,
    Headed,
    Expression,
    Arrow,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Frame {
    Paren { head: bool },
    Bracket,
    Brace(Brace),
    Template,
    Substitution,
    Tag,
    TypeArguments,
    Children,
}

#[derive(Default)]
struct Frames {
    top: Option<Rc<Opened>>,
    spare: Option<Rc<Opened>>,
}

struct Opened {
    frame: Frame,
    parent: Run,
    stamp: u64,
    below: Frames,
}

impl Clone for Frames {
    fn clone(&self) -> Self {
        Self {
            top: self.top.clone(),
            spare: None,
        }
    }
}

impl Frames {
    fn last(&self) -> Option<Frame> {
        self.top.as_ref().map(|opened| opened.frame)
    }

    fn stamp(&self) -> u64 {
        self.top.as_ref().map_or(0, |opened| opened.stamp)
    }

    fn push(&mut self, frame: Frame, parent: Run) {
        let kind = match frame {
            Frame::Paren { head } => 1 + u64::from(head),
            Frame::Bracket => 3,
            Frame::Brace(brace) => 4 + brace as u64,
            Frame::Template => 10,
            Frame::Substitution => 11,
            Frame::Tag => 12,
            Frame::TypeArguments => 13,
            Frame::Children => 14,
        };
        let stamp = (self.stamp().rotate_left(5) ^ kind).wrapping_mul(0x517c_c1b7_2722_0a95);
        let below = Frames {
            top: self.top.take(),
            spare: None,
        };
        let opened = Opened {
            frame,
            parent,
            stamp,
            below,
        };
        let mut spare = self.spare.take();
        self.top = Some(match spare.as_mut().and_then(Rc::get_mut) {
            Some(slot) => {
                *slot = opened;
                spare.expect("the spare is there")
            }
            None => Rc::new(opened),
        });
    }

    fn pop(&mut self) -> Option<(Frame, Run)> {
        let mut top = self.top.take()?;
        let popped = (top.frame, top.parent);
        let unique = Rc::get_mut(&mut top).map(|opened| opened.below.top.take());
        self.top = match unique {
            Some(below) => {
                self.spare = Some(top);
                below
            }
            None => top.below.top.clone(),
        };

        Some(popped)
    }
}

impl PartialEq for Frames {
    fn eq(&self, other: &Self) -> bool {
        if self.stamp() != other.stamp() {
            return false;
        }

        let (mut mine, mut theirs) = (&self.top, &other.top);
        loop {
            match (mine, theirs) {
                (None, None) => return true,
                (Some(a), Some(b)) if Rc::ptr_eq(a, b) => return true,
                (Some(a), Some(b)) if (a.frame, a.parent) == (b.frame, b.parent) => {
                    (mine, theirs) = (&a.below.top, &b.below.top);
                }
                _ => return false,
            }
        }
    }
}

// A frame stack can be thousands deep, and the default drop recurses once per frame.
impl Drop for Frames {
    fn drop(&mut self) {
        let mut next = self.top.take();
        while let Some(opened) = next {
            next = Rc::try_unwrap(opened)
                .ok()
                .and_then(|mut opened| opened.below.top.take());
        }
    }
}

type Regexes = Rc<[(usize, usize, usize)]>;

#[derive(Clone, Copy)]
struct Brackets {
    lift: i64,
    floor: i64,
}

impl Brackets {
    const NONE: Self = Self {
        lift: i64::MIN / 2,
        floor: 0,
    };

    fn after(self, byte: u8) -> Self {
        match byte {
            b'(' | b'[' | b'{' => Self {
                lift: self.lift.max(0) + 1,
                floor: self.floor,
            },
            b')' | b']' | b'}' => Self {
                lift: self.lift - 1,
                floor: self.lift.max(self.floor),
            },
            _ => self,
        }
    }

    fn peak(self) -> usize {
        self.lift.max(self.floor).max(0) as usize
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Run {
    units: usize,
    heads: usize,
    angles: usize,
    declaring: bool,
    guessed: bool,
}

impl Run {
    fn depth(&self) -> usize {
        self.units + self.heads * UNITS_PER_LEVEL
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Opening {
    Element,
    Parameters,
    Either,
}

#[derive(Clone, Copy, PartialEq)]
enum Prev {
    Statement,
    Operator,
    Operand,
}

#[derive(Clone, Copy, PartialEq)]
enum Word {
    Transparent,
    Statement,
    Declaring,
    Do,
    Head,
    Of,
    Recursing,
    Light,
    Contextual,
    Operand,
    Expecting,
    Name,
}

impl Word {
    fn of(word: &[u8]) -> Self {
        match word {
            b"export" | b"async" | b"declare" | b"abstract" => Self::Transparent,
            b"else" | b"try" | b"finally" | b"catch" | b"default" => Self::Statement,
            b"function" | b"class" | b"interface" | b"enum" | b"namespace" | b"module" => {
                Self::Declaring
            }
            b"do" => Self::Do,
            b"if" | b"while" | b"for" | b"with" | b"switch" => Self::Head,
            b"of" => Self::Of,
            b"await" | b"new" | b"yield" | b"extends" => Self::Recursing,
            b"typeof" | b"void" | b"delete" | b"in" | b"instanceof" => Self::Light,
            b"keyof" | b"readonly" | b"unique" | b"infer" | b"as" | b"satisfies" => {
                Self::Contextual
            }
            b"this" | b"super" | b"null" | b"true" | b"false" => Self::Operand,
            b"return" | b"throw" | b"case" | b"const" | b"var" | b"import" => Self::Expecting,
            _ => Self::Name,
        }
    }
}

fn continuing(word: &[u8]) -> bool {
    matches!(
        word,
        b"in" | b"instanceof" | b"as" | b"satisfies" | b"extends" | b"implements" | b"is"
    )
}

fn operator_length(rest: &[u8]) -> usize {
    match rest {
        [b'>', b'>', b'>', b'=', ..] => 4,
        [b'.', b'.', b'.', ..]
        | [b'=' | b'!', b'=', b'=', ..]
        | [b'*', b'*', b'=', ..]
        | [b'<', b'<', b'=', ..]
        | [b'>', b'>', b'=' | b'>', ..]
        | [b'&', b'&', b'=', ..]
        | [b'|', b'|', b'=', ..]
        | [b'?', b'?', b'=', ..] => 3,
        [b'?', b'.', digit, ..] if digit.is_ascii_digit() => 1,
        [b'=', b'>' | b'=', ..]
        | [
            b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^',
            b'=',
            ..,
        ]
        | [b'&', b'&', ..]
        | [b'|', b'|', ..]
        | [b'?', b'?' | b'.', ..]
        | [b'*', b'*', ..]
        | [b'+', b'+', ..]
        | [b'-', b'-', ..]
        | [b'<', b'<', ..]
        | [b'>', b'>', ..] => 2,
        _ => 1,
    }
}

fn recursing_operator(token: &[u8]) -> bool {
    matches!(
        token,
        b"..."
            | b"**="
            | b"<<="
            | b">>="
            | b">>>="
            | b"&&="
            | b"||="
            | b"??="
            | b"=>"
            | b"**"
            | b"+="
            | b"-="
            | b"*="
            | b"/="
            | b"%="
            | b"&="
            | b"|="
            | b"^="
            | b"="
            | b"?"
            | b":"
            | b"<"
    )
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'\\') || byte >= 0x80
}

const RESERVED: &[&[u8]] = &[
    b"break",
    b"case",
    b"catch",
    b"class",
    b"const",
    b"continue",
    b"debugger",
    b"default",
    b"delete",
    b"do",
    b"else",
    b"enum",
    b"export",
    b"extends",
    b"false",
    b"finally",
    b"for",
    b"function",
    b"if",
    b"import",
    b"in",
    b"instanceof",
    b"new",
    b"null",
    b"return",
    b"super",
    b"switch",
    b"this",
    b"throw",
    b"true",
    b"try",
    b"typeof",
    b"var",
    b"void",
    b"while",
    b"with",
];

fn starts_name(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$' | b'\\') || byte >= 0x80
}

fn unescape(word: &[u8]) -> Option<Vec<u8>> {
    let mut plain = Vec::with_capacity(word.len());
    let mut at = 0;
    while let Some(&byte) = word.get(at) {
        if byte != b'\\' {
            plain.push(byte);
            at += 1;
            continue;
        }

        let (digits, length) = match &word[at + 1..] {
            [b'u', b'{', rest @ ..] => {
                let close = rest.iter().position(|&byte| byte == b'}')?;
                (&rest[..close], close + 3)
            }
            [b'u', rest @ ..] => (rest.get(..4)?, 5),
            _ => return None,
        };

        if digits.is_empty() || !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }

        let code = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
        let decoded = u8::try_from(code).ok()?;
        if !(decoded.is_ascii_alphanumeric() || matches!(decoded, b'_' | b'$')) {
            return None;
        }

        plain.push(decoded);
        at += 1 + length;
    }

    Some(plain)
}

#[derive(Clone)]
struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
    jsx: bool,
    typescript: bool,
    module: bool,
    cap: usize,
    frames: Frames,
    base: usize,
    run: Run,
    prev: Prev,
    property: bool,
    head: bool,
    ended: bool,
    arrow_closed: bool,
    head_closed: bool,
    divides: bool,
    exported: bool,
    expects: bool,
    newline: bool,
    openers: Vec<usize>,
    regexes: Option<(usize, usize, Regexes)>,
    region_peak: usize,
    ambiguous: bool,
    forks: Vec<Scanner<'a>>,
    stalled: bool,
    alone: bool,
}

impl<'a> Scanner<'a> {
    fn peek(&self, offset: usize) -> u8 {
        self.bytes.get(self.at + offset).copied().unwrap_or(0)
    }

    fn line_break(&self, at: usize) -> usize {
        match self.bytes.get(at..at + 3) {
            Some([0xe2, 0x80, 0xa8 | 0xa9]) => 3,
            _ => usize::from(matches!(self.bytes.get(at), Some(b'\n' | b'\r'))),
        }
    }

    fn space(&self, at: usize) -> usize {
        match self.bytes.get(at..at + 3) {
            Some([0xe2, 0x80, 0x80..=0x8a | 0xaf] | [0xe2, 0x81, 0x9f] | [0xe3, 0x80, 0x80])
            | Some([0xe1, 0x9a, 0x80] | [0xef, 0xbb, 0xbf]) => 3,
            _ => match self.bytes.get(at..at + 2) {
                Some([0xc2, 0xa0]) => 2,
                _ => usize::from(matches!(
                    self.bytes.get(at),
                    Some(b' ' | b'\t' | 0x0b | 0x0c)
                )),
            },
        }
    }

    fn charged(&self, depth: usize) -> bool {
        let read = self.at.min(self.bytes.len());
        debug_assert!(
            depth <= read * UNITS_PER_LEVEL,
            "{depth} units after {read} bytes"
        );
        depth > self.cap * UNITS_PER_LEVEL
    }

    fn crossed(&self, start: usize) -> Option<usize> {
        self.charged(self.base + self.run.depth()).then_some(start)
    }

    fn count(&mut self, units: usize, start: usize) -> Option<usize> {
        self.run.units += units;
        self.crossed(start)
    }

    fn own(frame: Frame) -> usize {
        if frame == Frame::Brace(Brace::Headed) {
            0
        } else {
            UNITS_PER_LEVEL
        }
    }

    fn push(&mut self, frame: Frame, start: usize) -> Option<usize> {
        self.base += Self::own(frame) + self.run.depth();
        self.frames.push(frame, self.run);
        self.run = Run::default();
        self.crossed(start)
    }

    fn clear_run(&mut self) {
        self.openers.truncate(self.openers.len() - self.run.angles);
        self.run = Run::default();
    }

    fn region(&mut self, from: usize) {
        let (mut open, mut peak) = (0usize, 0usize);
        for &byte in &self.bytes[from..self.at.min(self.bytes.len())] {
            match byte {
                b'(' | b'[' | b'{' => {
                    open += 1;
                    peak = peak.max(open);
                }
                b')' | b']' | b'}' => open = open.saturating_sub(1),
                _ => {}
            }
        }

        self.region_peak = self.region_peak.max(peak);
    }

    fn inert(&mut self, from: usize) {
        if !self.alone {
            self.region(from);
        }
    }

    fn region_crossed(&mut self, start: usize) -> Option<usize> {
        let peak = std::mem::take(&mut self.region_peak);
        self.charged(self.base + self.run.depth() + peak * UNITS_PER_LEVEL)
            .then_some(start)
    }

    fn pop(&mut self) -> Frame {
        self.clear_run();
        let (frame, parent) = self.frames.pop().expect("a frame is open");
        self.base -= Self::own(frame) + parent.depth();
        self.run = parent;
        frame
    }

    fn step(&mut self) -> Option<usize> {
        match self.frames.last() {
            Some(Frame::Template) => self.template_text(),
            Some(Frame::Tag) => self.tag(),
            Some(Frame::Children) => self.children(),
            _ => self.code(),
        }
    }

    fn covers(&self, other: &Self) -> bool {
        let Self {
            bytes: _,
            at,
            jsx: _,
            typescript: _,
            module: _,
            cap: _,
            frames,
            base,
            run,
            prev,
            property,
            head,
            ended,
            arrow_closed,
            head_closed,
            divides,
            exported,
            expects,
            newline,
            openers,
            regexes: _,
            region_peak,
            ambiguous,
            forks: _,
            stalled: _,
            alone: _,
        } = other;
        (self.at, self.base, self.region_peak) == (*at, *base, *region_peak)
            && (self.prev, self.ambiguous) == (*prev, *ambiguous)
            && (
                self.run.heads,
                self.run.angles,
                self.run.declaring,
                self.run.guessed,
            ) == (run.heads, run.angles, run.declaring, run.guessed)
            && self.run.units >= run.units
            && [
                self.property,
                self.head,
                self.ended,
                self.arrow_closed,
                self.head_closed,
                self.divides,
                self.exported,
                self.expects,
                self.newline,
            ] == [
                *property,
                *head,
                *ended,
                *arrow_closed,
                *head_closed,
                *divides,
                *exported,
                *expects,
                *newline,
            ]
            && self.frames == *frames
            && self.openers == *openers
    }

    fn skip_line(&mut self) {
        while self.at < self.bytes.len() && self.line_break(self.at) == 0 {
            self.at += 1;
        }
    }

    fn skip_space(&mut self) {
        while self.at < self.bytes.len() {
            let breaking = self.line_break(self.at);
            if breaking > 0 {
                self.newline = true;
                self.at += breaking;
                continue;
            }

            let spacing = self.space(self.at);
            if spacing > 0 {
                self.at += spacing;
                continue;
            }

            let html = self.html_comment(self.at, self.newline);
            let from = self.at;
            match (self.peek(0), self.peek(1)) {
                (b'/', b'/') => self.skip_line(),
                (b'/', b'*') => self.skip_block_comment(),
                _ if html => self.skip_line(),
                _ => return,
            }

            if html {
                self.region(from);
            } else {
                self.inert(from);
            }
        }
    }

    fn skip_block_comment(&mut self) {
        self.at += 2;
        while self.at < self.bytes.len() {
            if self.peek(0) == b'*' && self.peek(1) == b'/' {
                self.at += 2;
                return;
            }

            let breaking = self.line_break(self.at);
            self.newline |= breaking > 0;
            self.at += breaking.max(1);
        }
    }

    fn skip_quoted(&mut self, quote: u8, escapes: bool) {
        self.at += 1;
        while self.at < self.bytes.len() {
            let byte = self.peek(0);
            if byte == quote {
                self.at += 1;
                return;
            }

            if !escapes {
                self.at += 1;
                continue;
            }

            if byte == b'\\' {
                self.at += 1;
                let crlf = self.peek(0) == b'\r' && self.peek(1) == b'\n';
                self.at += if crlf {
                    2
                } else {
                    self.line_break(self.at).max(1)
                };
                continue;
            }

            if matches!(byte, b'\n' | b'\r') {
                return;
            }

            self.at += 1;
        }
    }

    fn regex_end(&mut self) -> Option<(usize, usize)> {
        let slash = self.at;
        let known = self
            .regexes
            .as_ref()
            .is_some_and(|(first, end, _)| (*first..*end).contains(&slash));
        if !known {
            let (end, found) = self.line_regexes(slash);
            self.regexes = Some((slash, end, found));
        }

        let (_, _, found) = self.regexes.as_ref().expect("the line is known");
        let index = found.binary_search_by_key(&slash, |&(at, ..)| at).ok()?;
        let (_, close, brackets) = found[index];
        Some((close, brackets))
    }

    fn line_regexes(&self, first: usize) -> (usize, Regexes) {
        let mut end = first;
        while end < self.bytes.len() && self.line_break(end) == 0 {
            end += 1;
        }

        let fail = (None, Brackets::NONE);
        let (mut outside_next, mut outside_after) = (fail, fail);
        let (mut inside_next, mut inside_after) = (fail, fail);
        let mut found = Vec::new();
        for at in (first..end).rev() {
            let byte = self.bytes[at];
            if let (true, (Some(close), brackets)) = (byte == b'/', outside_next) {
                found.push((at, close, brackets.peak()));
            }

            let (outside, inside) = match byte {
                b'/' => ((Some(at), Brackets::NONE), inside_next),
                b'[' => {
                    let opened = (inside_next.0, inside_next.1.after(byte));
                    (opened, opened)
                }
                b']' => {
                    let closed = (outside_next.0, outside_next.1.after(byte));
                    (closed, closed)
                }
                b'\\' if at + 1 >= end => (fail, fail),
                b'\\' => {
                    let escaped = self.bytes[at + 1];
                    (
                        (outside_after.0, outside_after.1.after(escaped)),
                        (inside_after.0, inside_after.1.after(escaped)),
                    )
                }
                _ => (
                    (outside_next.0, outside_next.1.after(byte)),
                    (inside_next.0, inside_next.1.after(byte)),
                ),
            };

            (outside_after, outside_next) = (outside_next, outside);
            (inside_after, inside_next) = (inside_next, inside);
        }

        found.reverse();
        (end, found.into())
    }

    #[cfg(test)]
    fn skip_regex(&mut self) -> bool {
        self.at += 1;
        let mut class = false;
        while self.at < self.bytes.len() && self.line_break(self.at) == 0 {
            match self.peek(0) {
                b'\\' if self.line_break(self.at + 1) > 0 => return false,
                b'\\' => self.at += 1,
                b'[' => class = true,
                b']' => class = false,
                b'/' if !class => {
                    self.at += 1;
                    self.word();
                    return true;
                }
                _ => {}
            }

            self.at += 1;
        }

        false
    }

    fn word_end(&self, mut at: usize) -> usize {
        while let Some(&byte) = self.bytes.get(at) {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') {
                at += 1;
                continue;
            }

            if !is_word_byte(byte) || self.space(at) > 0 || self.line_break(at) > 0 {
                break;
            }

            at += match self.bytes.get(at..at + 3) {
                Some([b'\\', b'u', b'{']) => {
                    let digits = self.bytes[at + 3..]
                        .iter()
                        .take_while(|byte| byte.is_ascii_hexdigit())
                        .count();
                    3 + digits + usize::from(self.bytes.get(at + 3 + digits) == Some(&b'}'))
                }
                _ if self.bytes[at] == b'\\' => 2,
                _ => 1,
            };
        }

        at.min(self.bytes.len())
    }

    fn word(&mut self) -> &'a [u8] {
        let start = self.at;
        self.at = self.word_end(start);
        &self.bytes[start..self.at]
    }

    fn begin(&mut self, continues: bool, resumes: bool) {
        let continues = std::mem::take(&mut self.expects) || continues;
        if std::mem::take(&mut self.ended) && !resumes {
            self.clear_run();
            self.prev = Prev::Statement;
        }

        if std::mem::take(&mut self.newline) && self.prev == Prev::Operand && !continues {
            self.clear_run();
            self.prev = Prev::Statement;
        }
    }

    fn code(&mut self) -> Option<usize> {
        self.skip_space();
        if let Some(crossed) = self.region_crossed(self.at) {
            return Some(crossed);
        }

        if self.at >= self.bytes.len() {
            return None;
        }

        let after_arrow = std::mem::take(&mut self.arrow_closed) && self.newline;
        let divides = std::mem::take(&mut self.divides);
        let head_closed = std::mem::take(&mut self.head_closed);
        let starts_operand = self.prev != Prev::Operand && !divides || after_arrow;
        let start = self.at;
        let byte = self.peek(0);
        let property = std::mem::take(&mut self.property);
        let head = std::mem::take(&mut self.head);
        let ambiguous =
            std::mem::take(&mut self.ambiguous) || self.newline && self.prev == Prev::Operand;

        if byte.is_ascii_digit() || byte == b'.' && self.peek(1).is_ascii_digit() {
            self.begin(false, false);
            self.at += 1;
            let number = self.word();
            if matches!(number.last(), Some(b'e' | b'E')) && matches!(self.peek(0), b'+' | b'-') {
                self.at += 1;
                self.word();
            }

            self.prev = Prev::Operand;
            return None;
        }

        if is_word_byte(byte) || byte == b'#' {
            self.at += usize::from(byte == b'#');
            let word = self.word();
            if property || byte == b'#' {
                self.begin(property, false);
                self.prev = Prev::Operand;
                return None;
            }

            return self.keyword(word, start, head);
        }

        match byte {
            b'\'' | b'"' => {
                self.begin(false, false);
                self.skip_quoted(byte, true);
                self.inert(start);
                self.prev = Prev::Operand;
                self.region_crossed(start)
            }
            b'`' => {
                self.begin(true, false);
                self.at += 1;
                if self.prev == Prev::Operand && self.count(1, start).is_some() {
                    return Some(start);
                }

                self.push(Frame::Template, start)
            }
            b'(' | b'[' => {
                self.begin(true, false);
                self.at += 1;
                if self.prev == Prev::Operand && self.count(1, start).is_some() {
                    return Some(start);
                }

                if byte == b'(' && head {
                    self.run.heads += 1;
                }

                self.prev = Prev::Operator;
                let frame = if byte == b'[' {
                    Frame::Bracket
                } else {
                    Frame::Paren { head }
                };

                self.push(frame, start)
            }
            b'{' => {
                self.begin(true, false);
                self.at += 1;
                let kind = match self.prev {
                    Prev::Statement if head_closed && self.run.heads > 0 => Brace::Headed,
                    Prev::Statement => Brace::Block,
                    Prev::Operand if self.run.declaring => {
                        self.run.declaring = false;
                        if std::mem::take(&mut self.run.guessed) {
                            Brace::Guessed
                        } else {
                            Brace::Block
                        }
                    }
                    Prev::Operator if self.after_arrow(start) => Brace::Arrow,
                    _ => Brace::Expression,
                };

                self.prev = if matches!(kind, Brace::Block | Brace::Guessed | Brace::Headed) {
                    Prev::Statement
                } else {
                    Prev::Operator
                };

                self.push(Frame::Brace(kind), start)
            }
            b')' | b']' | b'}' => {
                self.begin(true, false);
                self.at += 1;
                self.close(byte);
                None
            }
            b',' => {
                self.begin(true, false);
                self.at += 1;
                self.prev = Prev::Operator;
                self.run.units = match (self.run.angles, self.openers.last()) {
                    (1.., Some(&before)) => before + UNITS_PER_LEVEL,
                    _ => 0,
                };
                None
            }
            b';' => {
                self.begin(true, false);
                self.at += 1;
                self.prev = Prev::Operator;
                match self.frames.last() {
                    Some(Frame::Paren { head }) => {
                        self.stalled |= !head;
                        self.clear_run();
                    }
                    Some(Frame::Bracket) => {
                        self.stalled = true;
                        self.ended = true;
                    }
                    _ => self.ended = true,
                }

                None
            }
            b'/' if (starts_operand || ambiguous) && self.regex_end().is_some() => {
                if ambiguous {
                    let mut divided = self.clone();
                    if let Some(crossed) = divided.operator(start) {
                        return Some(crossed);
                    }

                    self.forks.push(divided);
                }

                self.begin(false, false);
                let (close, brackets) = self.regex_end().expect("the regex ends");
                self.at = close + 1;
                self.word();
                self.region_peak = self.region_peak.max(brackets);
                self.prev = Prev::Operand;
                self.region_crossed(start)
            }
            b'<' if self.jsx && starts_operand => self.angle(start),
            b'>' if matches!(self.frames.last(), Some(Frame::TypeArguments)) => {
                self.begin(true, false);
                self.at += 1;
                if self.run.angles == 0 {
                    self.pop();
                } else {
                    self.close_angles(1);
                    self.prev = Prev::Operand;
                }

                None
            }
            b'@' => {
                self.begin(false, false);
                self.at += 1;
                self.prev = Prev::Operator;
                None
            }
            _ => self.operator(start),
        }
    }

    fn spaced(&self, start: usize, length: usize) -> bool {
        let before = start.checked_sub(1).map(|at| self.bytes[at]);
        let after = self.bytes.get(start + length).copied();
        [before, after]
            .iter()
            .all(|byte| byte.is_some_and(|byte| byte.is_ascii_whitespace()))
    }

    fn close_angles(&mut self, count: usize) {
        for _ in 0..count.min(self.run.angles) {
            self.run.units = self.openers.pop().expect("an angle is open");
            self.run.angles -= 1;
        }
    }

    fn after_arrow(&self, start: usize) -> bool {
        self.bytes[..start].trim_ascii_end().ends_with(b"=>")
    }

    fn operator(&mut self, start: usize) -> Option<usize> {
        let rest = &self.bytes[self.at..];
        let token = &rest[..operator_length(rest)];

        let update = token == b"++" || token == b"--";
        self.begin(!update, false);
        self.at += token.len();
        self.property = token == b"." || token == b"?.";
        self.run.declaring &= token != b"=";
        let mut closes_angle = false;
        match token {
            // oxc reads an unspaced `<` in TypeScript as type arguments, recursing until a `>` or the end of the file.
            b"<" | b"<<" if self.typescript && !self.spaced(start, token.len()) => {
                for _ in token {
                    self.openers.push(self.run.units);
                }

                self.run.angles += token.len();
            }
            b">" | b">>" | b">>>" => {
                closes_angle = self.run.angles > 0;
                self.close_angles(token.len());
                self.ambiguous = self.typescript;
            }
            _ => {}
        }

        let postfix = self.prev == Prev::Operand && (update || token == b"!" || self.property);
        if closes_angle {
            self.prev = Prev::Operand;
        } else if !postfix {
            self.prev = Prev::Operator;
        }

        let units = if recursing_operator(token) {
            UNITS_PER_LEVEL
        } else {
            1
        };

        self.count(units, start)
    }

    fn keyword(&mut self, word: &[u8], start: usize, head: bool) -> Option<usize> {
        if word.contains(&b'\\') {
            if let Some(plain) = unescape(word) {
                return self.keyword(&plain, start, head);
            }

            self.begin(false, false);
            self.prev = Prev::Operand;
            self.expects = true;
            return self.count(UNITS_PER_LEVEL, start);
        }

        let resumes = word == b"else";
        let exported = std::mem::take(&mut self.exported);
        self.begin(resumes || continuing(word), resumes);
        match Word::of(word) {
            Word::Transparent => {
                self.exported = word == b"export";
                self.divides = word != b"export";
                None
            }
            Word::Statement => {
                if word == b"default" {
                    self.clear_run();
                }

                self.head = word == b"catch";
                self.prev = if exported {
                    Prev::Operator
                } else {
                    Prev::Statement
                };
                None
            }
            Word::Declaring => {
                if self.prev == Prev::Statement {
                    self.run.declaring = true;
                    self.run.guessed = !matches!(word, b"function" | b"class" | b"enum");
                }
                self.divides = !matches!(word, b"function" | b"class" | b"enum");
                self.prev = Prev::Operator;
                None
            }
            Word::Do => {
                self.prev = Prev::Statement;
                self.run.heads += 1;
                self.crossed(start)
            }
            Word::Head => {
                self.head = true;
                self.prev = Prev::Statement;
                None
            }
            Word::Of => {
                self.prev = Prev::Operator;
                self.ambiguous = true;
                None
            }
            Word::Recursing => {
                self.head = head && word == b"await";
                self.divides = matches!(word, b"await" | b"yield");
                self.ambiguous = self.divides;
                self.prev = Prev::Operator;
                self.count(UNITS_PER_LEVEL, start)
            }
            Word::Light => {
                self.prev = Prev::Operator;
                self.count(1, start)
            }
            Word::Contextual => {
                let units = if matches!(word, b"as" | b"satisfies") {
                    1
                } else {
                    UNITS_PER_LEVEL
                };

                self.prev = Prev::Operand;
                self.expects = true;
                self.count(units, start)
            }
            Word::Expecting if self.prev != Prev::Operand => {
                if word == b"case" {
                    self.clear_run();
                }

                self.prev = Prev::Operator;
                None
            }
            Word::Operand | Word::Expecting | Word::Name => {
                self.prev = Prev::Operand;
                None
            }
        }
    }

    fn close(&mut self, byte: u8) {
        let closes = matches!(
            (self.frames.last(), byte),
            (Some(Frame::Paren { .. }), b')')
                | (Some(Frame::Bracket), b']')
                | (Some(Frame::Brace(_) | Frame::Substitution), b'}')
        );

        if !closes {
            self.stalled = true;
            return;
        }

        self.prev = match self.pop() {
            Frame::Paren { head: true } => {
                self.head_closed = true;
                Prev::Statement
            }
            Frame::Brace(Brace::Guessed) => {
                self.ended = true;
                self.ambiguous = true;
                Prev::Statement
            }
            Frame::Brace(Brace::Block | Brace::Headed) => {
                self.ended = true;
                Prev::Statement
            }
            Frame::Brace(Brace::Arrow) => {
                self.arrow_closed = true;
                Prev::Operand
            }
            Frame::Brace(Brace::Expression) => {
                self.ambiguous = !matches!(self.frames.last(), Some(Frame::Tag | Frame::Children));
                Prev::Operand
            }
            _ => Prev::Operand,
        };
    }

    fn angle(&mut self, start: usize) -> Option<usize> {
        let opening = self.opening();
        if opening == Opening::Parameters {
            return self.operator(start);
        }

        if opening == Opening::Either {
            let mut parameters = self.clone();
            if let Some(crossed) = parameters.operator(start) {
                return Some(crossed);
            }

            self.forks.push(parameters);
        }

        self.begin(false, false);
        self.at += 1;
        self.push(Frame::Tag, start)
    }

    // Mirrors oxc's JSX override in `is_parenthesized_arrow_function_expression_worker`.
    fn opening(&self) -> Opening {
        let name = |from: usize| {
            let end = self.word_end(from);
            let raw = &self.bytes[from..end];
            let plain = match raw.contains(&b'\\') {
                true => unescape(raw),
                false => Some(raw.to_vec()),
            };
            (end, plain)
        };
        let is = |plain: &Option<Vec<u8>>, word: &[u8]| plain.as_deref() == Some(word);
        let byte = |at: usize| self.bytes.get(at).copied();
        let assigns =
            |at: usize| byte(at) == Some(b'=') && !matches!(byte(at + 1), Some(b'=' | b'>'));

        let mut at = self.skip_trivia(self.at + 1);
        match byte(at) {
            Some(b'>') => return Opening::Element,
            Some(first) if starts_name(first) => {}
            _ => return Opening::Parameters,
        }

        let (mut end, plain) = name(at);
        if is(&plain, b"const") {
            at = self.skip_trivia(end);
            if !byte(at).is_some_and(starts_name) {
                return Opening::Either;
            }

            end = self.word_end(at);
        }

        let third = self.skip_trivia(end);
        let (after, word) = name(third);
        if is(&word, b"extends") {
            let fourth = self.skip_trivia(after);
            return match byte(fourth) {
                Some(b'>') => Opening::Element,
                Some(b'/') if byte(fourth + 1) != Some(b'=') => Opening::Element,
                Some(b'=') if assigns(fourth) => Opening::Element,
                Some(first) if starts_name(first) => match name(fourth).1 {
                    Some(plain) if RESERVED.contains(&plain.as_slice()) => Opening::Parameters,
                    _ => Opening::Either,
                },
                _ => Opening::Parameters,
            };
        }

        if assigns(third) || byte(third) == Some(b',') {
            Opening::Parameters
        } else {
            Opening::Element
        }
    }

    fn skip_trivia(&self, mut at: usize) -> usize {
        let mut newline = false;
        while at < self.bytes.len() {
            let (breaking, spacing) = (self.line_break(at), self.space(at));
            if breaking > 0 {
                newline = true;
                at += breaking;
            } else if spacing > 0 {
                at += spacing;
            } else if self.bytes[at..].starts_with(b"//") || self.html_comment(at, newline) {
                while at < self.bytes.len() && self.line_break(at) == 0 {
                    at += 1;
                }
            } else if self.bytes[at..].starts_with(b"/*") {
                let end = self.bytes[at + 2..]
                    .windows(2)
                    .position(|pair| pair == b"*/")
                    .map_or(self.bytes.len(), |end| at + end + 4);
                newline |= (at..end).any(|inside| self.line_break(inside) > 0);
                at = end;
            } else {
                return at;
            }
        }

        at
    }

    fn html_comment(&self, at: usize, newline: bool) -> bool {
        let rest = &self.bytes[at..];
        rest.starts_with(b"<!--") && (newline || !self.module)
            || rest.starts_with(b"-->") && newline && !self.module
    }

    fn template_text(&mut self) -> Option<usize> {
        let from = self.at;
        while self.at < self.bytes.len() {
            match self.peek(0) {
                b'\\' => self.at += 2,
                b'`' => {
                    self.region(from);
                    self.at += 1;
                    self.pop();
                    self.prev = Prev::Operand;
                    return self.region_crossed(from);
                }
                b'$' if self.peek(1) == b'{' => {
                    self.region(from);
                    if let Some(crossed) = self.region_crossed(from) {
                        return Some(crossed);
                    }

                    let start = self.at;
                    self.at += 2;
                    self.prev = Prev::Operator;
                    return self.push(Frame::Substitution, start);
                }
                _ => self.at += 1,
            }
        }

        self.region(from);
        self.region_crossed(from)
    }

    fn tag(&mut self) -> Option<usize> {
        let mut newline = false;
        while self.at < self.bytes.len() {
            let start = self.at;
            let (breaking, spacing) = (self.line_break(start), self.space(start));
            if breaking > 0 || spacing > 0 {
                newline |= breaking > 0;
                self.at += breaking.max(spacing);
                continue;
            }

            if self.html_comment(start, newline) {
                self.skip_line();
                continue;
            }

            let was = std::mem::take(&mut newline);
            match self.peek(0) {
                quote @ (b'"' | b'\'') => {
                    self.skip_quoted(quote, false);
                    self.region(start);
                    if let Some(crossed) = self.region_crossed(start) {
                        return Some(crossed);
                    }
                }
                b'/' if self.peek(1) == b'/' => self.skip_line(),
                b'/' if self.peek(1) == b'*' => {
                    self.skip_block_comment();
                    newline = was || (start..self.at).any(|at| self.line_break(at) > 0);
                }
                b'(' | b'[' => {
                    self.at += 1;
                    if self.count(UNITS_PER_LEVEL, start).is_some() {
                        return Some(start);
                    }
                }
                b')' | b']' => {
                    self.at += 1;
                    self.run.units = self.run.units.saturating_sub(UNITS_PER_LEVEL);
                }
                b'{' => {
                    self.at += 1;
                    self.prev = Prev::Operator;
                    return self.push(Frame::Brace(Brace::Expression), start);
                }
                b'<' if self.bytes[..start].trim_ascii_end().ends_with(b"=") => {
                    self.at += 1;
                    return self.push(Frame::Tag, start);
                }
                b'<' => {
                    self.at += 1;
                    self.prev = Prev::Operator;
                    return self.push(Frame::TypeArguments, start);
                }
                b'/' if self.peek(1) == b'>' => {
                    self.at += 2;
                    self.pop();
                    self.prev = Prev::Operand;
                    return None;
                }
                b'>' => {
                    self.at += 1;
                    if let Some((_, parent)) = self.frames.pop() {
                        self.frames.push(Frame::Children, parent);
                    }

                    return None;
                }
                _ => self.at += 1,
            }
        }

        None
    }

    fn children(&mut self) -> Option<usize> {
        while self.at < self.bytes.len() {
            let start = self.at;
            match self.peek(0) {
                b'{' => {
                    self.at += 1;
                    self.prev = Prev::Operator;
                    return self.push(Frame::Brace(Brace::Expression), start);
                }
                b'<' if self.peek(1) == b'/' => {
                    while self.at < self.bytes.len() && self.peek(0) != b'>' {
                        self.at += 1;
                    }

                    self.at += 1;
                    self.pop();
                    self.prev = Prev::Operand;
                    return None;
                }
                b'<' => {
                    self.at += 1;
                    return self.push(Frame::Tag, start);
                }
                b'>' | b'}' => {
                    self.at += 1;
                    self.stalled = true;
                    return None;
                }
                b'(' | b'[' => {
                    self.at += 1;
                    if self.count(UNITS_PER_LEVEL, start).is_some() {
                        return Some(start);
                    }
                }
                b')' | b']' => {
                    self.at += 1;
                    self.run.units = self.run.units.saturating_sub(UNITS_PER_LEVEL);
                }
                _ => self.at += 1,
            }
        }

        None
    }
}

fn scanner(text: &str, source: SourceType, cap: usize) -> Scanner<'_> {
    let mut scanner = Scanner {
        bytes: text.as_bytes(),
        at: 0,
        jsx: source.is_jsx(),
        typescript: source.is_typescript(),
        module: source.is_module(),
        cap,
        frames: Frames::default(),
        base: 0,
        run: Run::default(),
        prev: Prev::Statement,
        property: false,
        head: false,
        ended: false,
        arrow_closed: false,
        head_closed: false,
        divides: false,
        exported: false,
        expects: false,
        newline: false,
        openers: Vec::new(),
        regexes: None,
        region_peak: 0,
        ambiguous: false,
        forks: Vec::new(),
        stalled: false,
        alone: true,
    };

    if scanner.bytes.starts_with(b"#!") {
        scanner.skip_line();
    }

    scanner.newline = true;
    scanner
}

enum Next<'a> {
    Step(Scanner<'a>),
    Crossed(usize),
    Done,
}

#[cold]
fn next<'a>(mut reading: Scanner<'a>, others: &mut Vec<Scanner<'a>>) -> Next<'a> {
    let at = reading.at;
    let mut arrivals = std::mem::take(&mut reading.forks);
    // oxc stops at the first token it cannot place, so a reading that reaches one can go while another stays.
    let stopped = reading.stalled && !(others.is_empty() && arrivals.is_empty());
    if at < reading.bytes.len() && !stopped {
        arrivals.push(reading);
    }

    for arrival in arrivals {
        if others.iter().any(|kept| kept.covers(&arrival)) {
            continue;
        }

        others.retain(|kept| !arrival.covers(kept));
        others.push(arrival);
    }

    if others.len() > MOST_READINGS {
        return Next::Crossed(at);
    }

    match (0..others.len()).min_by_key(|&index| others[index].at) {
        Some(behind) => Next::Step(others.swap_remove(behind)),
        None => Next::Done,
    }
}

pub fn crossing(text: &str, source: SourceType, cap: usize) -> Option<usize> {
    // Sound only while no byte adds more than one level, which `charged` asserts in debug builds.
    if text.len() <= cap {
        return None;
    }

    scan(text, source, cap)
}

fn scan(text: &str, source: SourceType, cap: usize) -> Option<usize> {
    let mut reading = scanner(text, source, cap);
    let mut others = Vec::new();
    loop {
        if reading.at < reading.bytes.len() {
            reading.stalled = false;
            // A reading can be dropped at a token oxc cannot place, so a string or comment frees its brackets only while no other reading may take that text for code.
            reading.alone = others.is_empty();
            if let Some(crossed) = reading.step() {
                return Some(crossed);
            }

            if others.is_empty() && reading.forks.is_empty() {
                continue;
            }
        }

        match next(reading, &mut others) {
            Next::Step(behind) => reading = behind,
            Next::Crossed(at) => return Some(at),
            Next::Done => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::process_file;
    use crate::engine::types::Mode;
    use crate::languages::FALLBACK_LAYOUT;
    use crate::languages::javascript::parse::source_type;

    type Nested = fn(usize) -> String;

    const ACCEPTANCE: [(&str, Nested); 7] = [
        ("a.ts", |n| {
            format!("const a = {}0{};\n", "[".repeat(n), "]".repeat(n))
        }),
        ("a.ts", |n| {
            format!("const a = {}0{};\n", "(".repeat(n), ")".repeat(n))
        }),
        ("a.ts", |n| {
            format!("{}b();\n{}", "if (a) {\n".repeat(n), "}\n".repeat(n))
        }),
        ("a.ts", |n| {
            format!("const a = {}0{};\n", "{ a: ".repeat(n), " }".repeat(n))
        }),
        ("a.ts", |n| {
            format!("const a = {}0{};\n", "`${".repeat(n), "}`".repeat(n))
        }),
        ("a.tsx", |n| {
            format!("const a = {}x{};\n", "<a>".repeat(n), "</a>".repeat(n))
        }),
        ("a.ts", |n| format!("const a = {}b;\n", "!".repeat(n))),
    ];

    fn deep(text: &str) -> bool {
        crossing(text, source_type("a.tsx"), 3).is_some()
    }

    #[test]
    fn every_acceptance_shape_a_million_deep_crosses_the_cap() {
        for (path, nested) in ACCEPTANCE {
            let text = nested(1_000_000);
            let start = crossing(&text, source_type(path), MOST_LEVELS).expect("crosses the cap");
            assert!(start < 200_000, "{path} crossed late, at {start}");
        }
    }

    #[test]
    fn a_file_at_the_cap_passes_and_one_level_more_does_not() {
        let nested = ACCEPTANCE[0].1;
        let ts = source_type("a.ts");
        assert_eq!(crossing(&nested(9), ts, 10), None);
        assert_eq!(crossing(&nested(10), ts, 10), Some(19));
    }

    #[test]
    fn token_soup_never_charges_a_byte_more_than_one_level() {
        const TOKENS: &str = concat!(
            "if while for with switch do else try catch finally default function class ",
            "interface enum namespace module export async declare abstract await new yield ",
            "extends of typeof void delete in instanceof keyof readonly unique infer as ",
            "satisfies this super null true false return throw case const var import ",
            "implements is x T é \\u0061 \\u{69}f \\x 1 1e+1 .5 >>>= ... === !== **= <<= >>= ",
            ">>> &&= ||= ??= => == != <= >= && || ?? ?. ?.5 ** ++ -- += -= *= /= %= &= |= ^= ",
            "<< >> = ? : < > + - * / % & | ^ ! ~ . , ; @ # ( ) [ ] { } ' \" ` ${ </ /> // /* ",
            "*/ <!-- --> \\ /((((/ /[((]/ '(((( \"(((( `(((( /*((((*/ //(((( <!--(((( <a( <a{ ",
        );
        const GAPS: &[&str] = &["", "", "", " ", "\n", "\u{2028}", "\u{a0}"];

        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };

        let tokens: Vec<&str> = TOKENS.split_whitespace().collect();
        for _ in 0..20_000 {
            let mut text = String::new();
            for _ in 0..next(120) + 1 {
                text.push_str(tokens[next(tokens.len())]);
                text.push_str(GAPS[next(GAPS.len())]);
            }

            for path in ["a.ts", "a.tsx", "a.js"] {
                scan(&text, source_type(path), usize::MAX / UNITS_PER_LEVEL);
            }
        }
    }

    #[test]
    fn text_no_longer_than_the_cap_skips_the_scan() {
        let forking = format!("x = a\n{}", "a\n/(/g\n".repeat(40));
        let ts = source_type("a.ts");
        assert!(scan(&forking, ts, MOST_LEVELS).is_some());
        assert_eq!(crossing(&forking, ts, MOST_LEVELS), None);
    }

    #[test]
    fn brackets_in_strings_and_comments_are_free() {
        for text in [
            "x = '[[[[[[[[';\n",
            "x = \"((((((((\";\n",
            "// [[[[[[[[\nx;\n",
            "/* [[[[[[[[ */ x;\n",
            "/* [[[[\n[[[[ */ x;\n",
            "x = 'a\\\n[[[[[[[[';\n",
            "x = 'a\\\r\n[[[[[[[[';\n",
        ] {
            assert!(!deep(text), "{text:?} was charged");
            assert!(deep(&format!("{text}[[[[0]]]];\n")), "{text:?} hid code");
        }

        let quoted = format!("const s = '{}';\nif(a) {{ b(); }}\n", "(".repeat(10_002));
        assert_eq!(crossing(&quoted, source_type("a.ts"), MOST_LEVELS), None);
    }

    #[test]
    fn a_string_read_beside_another_reading_keeps_its_charge() {
        for text in [
            "x = a\n/'/ } [[[[[[0]]]]]]';\n",
            "x = a\n/'/ } \\\n[[[[[[0]]]]]]';\n",
            "x = a\n/'/ } // [[[[[[\n';\n",
        ] {
            assert!(deep(text), "{text:?} hid its nesting");
        }
    }

    #[test]
    fn brackets_in_templates_regexes_and_jsx_attributes_count_only_inside_them() {
        for line in [
            "x = `[[[`;\n",
            "x = /[[[]/;\n",
            "x = /\\[[[]/g;\n",
            "x = <a b=\"[[[\" />;\n",
        ] {
            let text = line.repeat(1_000);
            assert!(
                !crossing(&text, source_type("a.tsx"), 5).is_some(),
                "{line:?} added up"
            );
            assert!(deep(&line.replace("[[[", "[[[[")), "{line:?}");
        }
    }

    #[test]
    fn text_that_only_looks_like_a_string_or_comment_hides_no_nesting() {
        for text in [
            "x = <a>don't</a>;\n[[[[[[0]]]]]];\n",
            "if (x) /'/.test(a);\n[[[[[[0]]]]]];\n",
            "{} /'/.test(a); [[[[[[0]]]]]];\n",
            "x = a / b; y = '[' + [[[[[[0]]]]]];\n",
            "// a\u{2028}[[[[[[0]]]]]];\n",
            "// a\r[[[[[[0]]]]]];\n",
            "x = 'a\\\n[[[[[[';\n[[[[[[0]]]]]];\n",
            "x = <a>/* [[[[[[0]]]]]] */</a>;\n",
            "const s = '\u{2028}'; [[[[[[0]]]]]];\n",
            "const f = () => {}\n/'/.test(s), [[[[[[0]]]]]];\n",
            "let type = 2; x = type / [[[[[[0]]]]]] / 1;\n",
            "x = <Foo<Bar> />;\n[[[[[[0]]]]]];\n",
            "x = <a b=<c>x</c>>x</a>;\n[[[[[[0]]]]]];\n",
            "class A extends B<{}> {}\n/'/.test(s), [[[[[[0]]]]]];\n",
            "x = <C<P<P<P<P<B>>>>> />;\n",
            "type A = keyof\nkeyof\nkeyof\nkeyof\nB;\n",
            "x = <T /*x*/,>() => [[[[[[0]]]]]];\n",
            "x = <T /*x*/\u{a0},>() => [[[[[[0]]]]]];\n",
            "x = <T\u{2028},>() => [[[[[[0]]]]]];\n",
            "x = <T\u{a0}extends U>() => [[[[[[0]]]]]];\n",
            "x = <T\u{2028}extends U>() => [[[[[[0]]]]]];\n",
            "await /'/; x = [[[[[[0]]]]]];\n",
            "x = '/' + `${a}` + \"[[[[[[\";\n",
            "x = <Foo\n  // don't remove\n  bar={1}\n/>;\ny = [[[[[[0]]]]]];\n'q';\n",
            "x = <Foo\n  // a > b\n/>;\ny = [[[[[[0]]]]]];\n",
            "x = async / [[[[[[0]]]]]] / 1;\n",
            "x = module / [[[[[[0]]]]]] / 1;\n",
            "x = await / [[[[[[0]]]]]] / 1;\n",
            "export default {} / [[[[[[0]]]]]] / 1;\n",
            "for (a of /]/) x = [[[[[[0]]]]]];\n",
            "type R = <T>(x: T) => [[[[[[0]]]]]];\n",
        ] {
            assert!(deep(text), "{text:?} hid its nesting");
        }
    }

    #[test]
    fn a_slash_read_either_way_hides_no_chain() {
        for (before, after) in [
            ("function f<T, U>() {}\n/`/.test(s);\n", ""),
            ("function f(): A<B, C> {}\n/`/.test(s);\n", ""),
            ("class A<T, U> {}\n/`/.test(s);\n", ""),
            ("class A implements B, C {}\n/`/.test(s);\n", ""),
            ("interface A extends B, C {}\n/`/.test(s);\n", ""),
            ("export default function () {}\n/`/.test(s);\n", ""),
            ("export default class {}\n/`/.test(s);\n", ""),
            ("l: {}\n/`/.test(s);\n", ""),
            ("switch (a) { case 1: {}\n/`/.test(s);\n", "}\n"),
            ("switch (a) { default: {}\n/`/.test(s);\n", "}\n"),
            ("x = {} / `/ 1;\n", ""),
            ("async function g() { await /'/; ", "}\n"),
            ("async function g() { await /`/;\n", "}\n"),
            ("function* g() { yield /`/;\n", "}\n"),
            ("x = await / `/;\n", ""),
            ("await /`/;\n", ""),
            ("function f<T = U>() {}\n/`/.test(s);\n", ""),
            ("function f(): void {}\n/`/.test(s);\n", ""),
            ("x = class A\n{} /`/;`;\n", ""),
            ("x = function ()\n{} /`/;`;\n", ""),
            ("let x\n/`/.test(s);\n", ""),
            ("type A = B\n/`/.test(s);\n", ""),
            ("f<T>\n/`/.test(s);\n", ""),
            ("for (;;) { break\n/`/.test(s);\n", "}\n"),
            ("x = of /`/;`;\n", ""),
            ("a < b> /`/;`;\n", ""),
            ("for (;{} /`/;);`; ", ""),
            ("async function g() { x = {} / a[await /`/]; ", "}\n"),
            ("module.a, b as {} / `/`;\n", ""),
            ("module.a as {} / `/`;\n", ""),
        ] {
            let deep = |text: &str| crossing(text, source_type("a.ts"), 6).is_some();
            assert!(
                !deep(&format!("{before}x = a;\n{after}")),
                "{before:?} alone"
            );
            for link in ["a=>", "a ? b : "] {
                let text = format!("{before}x = {}a;\n{after}", link.repeat(8));
                assert!(deep(&text), "{text:?} hid its chain");
            }
        }
    }

    #[test]
    fn escaped_keywords_html_comments_and_tsx_elements_hide_no_chain() {
        for (path, before) in [
            ("a.js", "i\\u0066 (a) /`/.test(s);\n"),
            ("a.js", "\\u0069f (a) /`/.test(s);\n"),
            ("a.js", "i\\u{66} (a) /`/.test(s);\n"),
            ("a.tsx", "y = <T\\u{61},>() => 0;\n"),
            ("a.ts", "x;\n<!-- `\n"),
            ("a.ts", "x; <!-- `\n"),
            ("a.ts", "x;\n--> `\n"),
            ("a.mjs", "x;\n<!-- `\n"),
            ("a.mjs", "x;\n/*\n*/ <!-- `\n"),
            ("a.tsx", "y = <T extends>`</T>;\n"),
            ("a.tsx", "y = < T>`</T>;\n"),
            ("a.tsx", "y = < /*c*/ T>`</T>;\n"),
            ("a.tsx", "y = <T <!-- `\n,>() => 0;\n"),
            ("a.tsx", "y = <T <!-- `\n/>;\n"),
            ("a.tsx", "y = <T\n--> `\n/>;\n"),
            ("a.tsx", "y = <T /*\n*/ --> `\n/>;\n"),
            ("a.tsx", "y = <T\n--> `\n,>() => 0;\n"),
            ("a.tsx", "y = <T extends=\"\">`</T>;\n"),
            ("a.tsx", "y = <T extends U>`</T>;\n"),
            ("a.tsx", "y = <const T>`</const>;\n"),
            ("a.tsx", "y = <T == 1>`</T>;\n"),
        ] {
            let deep = |text: &str| crossing(text, source_type(path), 6).is_some();
            assert!(!deep(&format!("{before}x = a;\n")), "{before:?} alone");
            for link in ["a=>", "a ? b : "] {
                let text = format!("{before}x = {}a;\n", link.repeat(8));
                assert!(deep(&text), "{path} {text:?} hid its chain");
            }
        }
    }

    #[test]
    fn a_jsx_angle_opens_what_oxc_opens() {
        for (text, opening) in [
            ("<>", Opening::Element),
            ("<a>", Opening::Element),
            ("<a b>", Opening::Element),
            ("<a == b>", Opening::Element),
            ("<a => b>", Opening::Element),
            ("<a,>", Opening::Parameters),
            ("<a = b>", Opening::Parameters),
            ("<1>", Opening::Parameters),
            ("<T extends>", Opening::Element),
            ("<T extends/>", Opening::Element),
            ("<T extends />", Opening::Element),
            ("<T extends=\"\">", Opening::Element),
            ("<T extends == 1>", Opening::Parameters),
            ("<T extends /= 1>", Opening::Parameters),
            ("<T extends {}>", Opening::Parameters),
            ("<T extends null>", Opening::Parameters),
            ("<T extends n\\u0075ll>", Opening::Parameters),
            ("<T\\u{61},>", Opening::Parameters),
            ("<T ext\\u0065nds U>", Opening::Either),
            ("<T ext\\u{65}nds>", Opening::Element),
            ("<c\\u006fnst T,>", Opening::Parameters),
            ("<T extends 1>", Opening::Parameters),
            ("< T>", Opening::Element),
            ("< /*c*/ T>", Opening::Element),
            ("<\nT,>", Opening::Parameters),
            ("<T <!-- x\n,>", Opening::Parameters),
            ("<T\n--> x\n,>", Opening::Parameters),
            ("<T /*\n*/ --> x\n,>", Opening::Parameters),
            ("<T --> x\n,>", Opening::Element),
            ("<T extends \\u00e9>", Opening::Either),
            ("<T extends U>", Opening::Either),
            ("<T extends await>", Opening::Either),
            ("<T extends /* x */ U>", Opening::Either),
            ("<const T>", Opening::Element),
            ("<const T,>", Opening::Parameters),
            ("<const T extends U>", Opening::Either),
            ("<const>", Opening::Either),
        ] {
            assert!(
                opening == scanner(text, source_type("a.tsx"), 8).opening(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn an_escape_decodes_once() {
        assert_eq!(unescape(b"i\\u0066"), Some(b"if".to_vec()));
        assert_eq!(unescape(b"\\u{69}f"), Some(b"if".to_vec()));
        assert_eq!(unescape(b"\\u005cu0061"), None);
        assert_eq!(unescape(b"\\u00e9"), None);
        let word = format!("\\{}u0061", "u005c".repeat(100_000));
        let text = format!("x = {word};\n");
        assert_eq!(crossing(&text, source_type("a.js"), 8), None);
    }

    #[test]
    fn html_comment_markers_inside_module_code_stay_operators() {
        for text in [
            "x = a <!-- b, [[[[[[0]]]]]];\n",
            "x = a\n--> b, [[[[[[0]]]]]];\n",
        ] {
            assert!(
                crossing(text, source_type("a.mjs"), 3).is_some(),
                "{text:?} hid its nesting"
            );
        }
    }

    #[test]
    fn a_tsx_type_parameter_list_read_both_ways_stays_cheap() {
        let text = format!(
            "{}{}",
            "const f = <T extends U>(x: T) => x;\n".repeat(1_000),
            "a = b < c;\n".repeat(10_000)
        );
        assert_eq!(crossing(&text, source_type("a.tsx"), 8), None);
    }

    #[test]
    fn a_regex_after_a_certain_declaration_has_one_reading() {
        for declaration in ["class A {} ", "function f() {}\n", "enum E {} "] {
            let text = format!("{declaration}/{}/.test(s);\n", "a?".repeat(8));
            assert_eq!(crossing(&text, source_type("a.ts"), 6), None, "{text:?}");
        }
    }

    #[test]
    fn slashes_read_either_way_stay_cheap_in_ordinary_code() {
        for text in [
            format!(
                "async function g() {{\n{}}}\n",
                "  await /[(]/.test(s);\n".repeat(40)
            ),
            format!(
                "async function g() {{\n{}}}\n",
                "  await /{/.test(s);\n\n".repeat(20)
            ),
            format!(
                "{}{}",
                "x = {}\n/(/;\n".repeat(14),
                "a = b + c;\n".repeat(100_000)
            ),
            format!(
                "x = {}{}",
                "[".repeat(2_000),
                "/ b / c, a\n".repeat(100_000)
            ),
            format!(
                "async function f() {{ {}]/; }}\n",
                "await /'[\\]'/; ".repeat(16_000)
            ),
            format!(
                "async function f() {{ await /{{/ +(a / a, {}{}a{}); }}\n",
                "(".repeat(8_000),
                "a,".repeat(8_000),
                ")".repeat(8_000)
            ),
            format!("const r = {{}}\n{}", "x = a\n/ b / c;\n".repeat(10_000)),
            format!("x = [\n{}];\n", "  {} / 2 / 3,\n".repeat(10_000)),
        ] {
            assert_eq!(
                crossing(&text, source_type("a.ts"), MOST_LEVELS),
                None,
                "{}",
                &text[..40]
            );
        }
    }

    #[test]
    fn a_regex_ends_and_charges_from_a_slash_exactly_as_scanning_it_does() {
        let alphabet = *b"/[]\\a\n()";
        let mut text = Vec::new();
        for mut code in 0..alphabet.len().pow(6) {
            text.clear();
            for _ in 0..6 {
                text.push(alphabet[code % alphabet.len()]);
                code /= alphabet.len();
            }

            let source = std::str::from_utf8(&text).unwrap();
            let mut tabled = scanner(source, source_type("a.ts"), 8);
            for at in (0..text.len()).filter(|&at| text[at] == b'/') {
                let mut scanned = scanner(source, source_type("a.ts"), 8);
                scanned.at = at;
                let ends = scanned.skip_regex();
                tabled.at = at;
                let found = tabled.regex_end();
                assert_eq!(found.is_some(), ends, "{source:?} at {at}");
                let Some((close, brackets)) = found else {
                    continue;
                };

                let mut body = scanner(source, source_type("a.ts"), 8);
                body.at = close + 1;
                body.word();
                assert_eq!(body.at, scanned.at, "{source:?} at {at}");
                body.at = close + 1;
                body.region(at);
                assert_eq!(brackets, body.region_peak, "{source:?} at {at}");
            }
        }
    }

    #[test]
    fn the_nesting_acceptance_inputs_cross_the_cap() {
        let prefix = "function f<T, U>() {}\n/`/.test(s);\n";
        let arrows = |n: usize| format!("{prefix}x = {}a;\n", "a=>".repeat(n));
        let ternaries = format!("{prefix}x = {}a;\n", "a ? b : ".repeat(120_000));
        let awaited = format!(
            "async function g() {{ await /'/; x = {}a; }}\n",
            "a=>".repeat(120_000)
        );
        for text in [arrows(120_000), arrows(4_000_000), ternaries, awaited] {
            assert!(
                crossing(&text, source_type("deep.ts"), MOST_LEVELS).is_some(),
                "{}",
                &text[..60]
            );
        }
    }

    #[test]
    fn chains_and_runs_without_delimiters_count() {
        let light = |token: &str| format!("x = {}a;\n", token.repeat(64));
        for token in ["a + ", "a.", "!", "typeof ", "a && ", "a | "] {
            assert!(deep(&light(token)), "{token:?} chains were not counted");
        }

        assert!(deep(&format!("x = f{};\n", "()".repeat(64))));
        assert!(deep(&format!("x = {}B;\n", "new\u{2028}".repeat(4))));
        for text in [
            "x = a ? b : a ? b : c;\n",
            "x = a => a => a => a;\n",
            "x = <T,>() => <T,>() => <T,>() => <T,>() => 0;\n",
            "x = f<A, B>(<T, U>() => <T, U>() => <T, U>() => <T, U>() => 0);\n",
            "a = b = c = d = e;\n",
            "if (a) if (b) if (c) if (d) x();\n",
            "if (a) x(); else if (b) x(); else if (c) x(); else if (d) x();\n",
            "if (a) b, c; else if (a) b, c; else if (a) b, c; else if (a) b, c;\n",
            "type A = B<x, B<x, B<x, B<x, x>, x>, x>, x>;\n",
            "type A = key\\u006ff key\\u006ff key\\u006ff key\\u006ff B;\n",
            "type A = keyof keyof B<X, keyof keyof B<X, C>>;\n",
            "x = a ? () => {}\n : a ? () => {}\n : a ? () => {}\n : a ? () => {}\n : 0;\n",
            "if (a) {} else if (b) {} else if (c) {} else if (d) {}\n",
            "if (a) x()\nelse if (b) x()\nelse if (c) x()\nelse if (d) x()\n",
            "type A = keyof keyof keyof keyof B;\n",
        ] {
            assert!(deep(text), "{text:?} was not counted");
        }
    }

    #[test]
    fn comparison_lists_nest_only_where_type_arguments_can_open() {
        let text = format!("const a = [{}x];\n", "x<y,".repeat(20_000));
        for path in ["a.js", "a.jsx"] {
            assert_eq!(crossing(&text, source_type(path), 8), None, "{path}");
        }

        assert!(crossing(&text, source_type("a.ts"), MOST_LEVELS).is_some());
    }

    #[test]
    fn ordinary_long_files_stay_under_a_small_cap() {
        let repeated = |line: &str, times: usize| line.repeat(times);
        for text in [
            repeated("a = b\n", 10_000),
            repeated("if (a) { b() } ", 10_000),
            repeated("function f() {}\n", 10_000),
            format!("switch (a) {{\n{}}}\n", repeated("case 1:\n", 10_000)),
            format!(
                "class A {{\n{}}}\n",
                repeated("  m() { return 1 }\n", 10_000)
            ),
            format!("x = {{\n{}}};\n", repeated("  a: f(b),\n", 10_000)),
            format!("x = [\n{}];\n", repeated("  [1, 2],\n", 10_000)),
            format!(
                "x = new Map<string, number>([\n{}]);\n",
                repeated("  [a, b < c],\n", 10_000)
            ),
            format!("f({});\n", repeated("a > b, ", 10_000)),
            format!(
                "x = {{ first: a < b,\n{}}};\n",
                repeated("  k: 1,\n", 10_000)
            ),
            format!("x = {{ {} }};\n", repeated("if: 1, for: 2, ", 5_000)),
            format!("x = [\n{}];\n", repeated("  () => f<T>(),\n", 10_000)),
            format!(
                "x = {{\n{}}};\n",
                repeated("  r: (v: number) => v < 1,\n", 10_000)
            ),
            format!("enum F {{\n{}}}\n", repeated("  A = 1 << 2,\n", 10_000)),
            format!("x = {};\n", repeated("'(((' + ", 40)),
            format!("x = {};\n", repeated("'a' +\n", 100)),
            format!("const s = '\u{2028}';\n{}", repeated("[[[0]]];\n", 10)),
            format!(
                "x = <a>\n{}</a>;\n",
                repeated("  <b c={d}>(e)</b>\n", 10_000)
            ),
            format!("x = `\n{}`;\n", repeated("  ${a} (b [c\n", 10_000)),
        ] {
            assert_eq!(
                crossing(&text, source_type("a.tsx"), 8),
                None,
                "{}",
                &text[..40]
            );
        }

        let union = format!("type A =\n{};\n", repeated("  | 'a'\n", 10_000));
        assert_eq!(crossing(&union, source_type("a.ts"), cap(8 << 20)), None);
    }

    #[test]
    fn the_cap_follows_the_stack_up_to_the_most_levels() {
        assert_eq!(cap(1 << 30), MOST_LEVELS);
        assert_eq!(cap(64 << 20), (64 << 20) / (4 * BYTES_PER_LEVEL));
        assert_eq!(cap(8 << 20), (8 << 20) / (4 * BYTES_PER_LEVEL));
        let spawned = std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn(thread_cap)
            .unwrap()
            .join()
            .unwrap();
        assert!((cap(8 << 20)..cap(9 << 20)).contains(&spawned), "{spawned}");
    }

    #[test]
    fn every_acceptance_shape_at_the_cap_of_an_eight_mebibyte_thread_formats_there() {
        let stack = 8 << 20;
        let cap = cap(stack);
        for (path, nested) in ACCEPTANCE {
            let (mut low, mut high) = (0, cap + 1);
            while high - low > 1 {
                let middle = (low + high) / 2;
                if crossing(&nested(middle), source_type(path), cap).is_none() {
                    low = middle;
                } else {
                    high = middle;
                }
            }

            let text = nested(low);
            let result = std::thread::Builder::new()
                .stack_size(stack)
                .spawn(move || process_file(path, &text, Mode::Fix, false, FALLBACK_LAYOUT, None))
                .unwrap()
                .join()
                .unwrap();
            assert!(
                !result.parse_error,
                "{path} at depth {low}: {:?}",
                result.findings.first()
            );
        }
    }
}
