use std::cell::Cell;

use oxc_span::SourceType;

use crate::pool::thread_stack;

pub const MOST_LEVELS: usize = 10_000;

pub const BYTES_PER_LEVEL: usize = 3072;

const UNITS_PER_LEVEL: usize = 16;

const FALLBACK_STACK: usize = 1 << 19;

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

#[derive(Clone, Copy, Default)]
struct Run {
    units: usize,
    heads: usize,
    angles: usize,
    declaring: bool,
}

impl Run {
    fn depth(&self) -> usize {
        self.units + self.heads * UNITS_PER_LEVEL
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Prev {
    Statement,
    Operator,
    Operand,
}

const HEADS: &[&[u8]] = &[b"if", b"while", b"for", b"with", b"switch"];

const RECURSING: &[&[u8]] = &[b"await", b"new", b"yield", b"extends"];

const LIGHT: &[&[u8]] = &[b"typeof", b"void", b"delete", b"in", b"instanceof"];

const CONTEXTUAL: &[&[u8]] = &[
    b"keyof",
    b"readonly",
    b"unique",
    b"infer",
    b"as",
    b"satisfies",
];

const CONTINUING: &[&[u8]] = &[
    b"in",
    b"instanceof",
    b"as",
    b"satisfies",
    b"extends",
    b"implements",
    b"is",
];

const DECLARING: &[&[u8]] = &[
    b"function",
    b"class",
    b"interface",
    b"enum",
    b"namespace",
    b"module",
];

const TRANSPARENT: &[&[u8]] = &[b"export", b"async", b"declare", b"abstract"];

const STATEMENTS: &[&[u8]] = &[b"else", b"try", b"finally", b"catch", b"default"];

const OPERANDS: &[&[u8]] = &[b"this", b"super", b"null", b"true", b"false"];

const EXPECTING: &[&[u8]] = &[b"return", b"throw", b"case", b"const", b"var", b"import"];

const OPERATORS: &[&[u8]] = &[
    b">>>=", b"...", b"===", b"!==", b"**=", b"<<=", b">>=", b">>>", b"&&=", b"||=", b"??=", b"=>",
    b"==", b"!=", b"<=", b">=", b"&&", b"||", b"??", b"?.", b"**", b"++", b"--", b"+=", b"-=",
    b"*=", b"/=", b"%=", b"&=", b"|=", b"^=", b"<<", b">>",
];

const RECURSING_OPERATORS: &[&[u8]] = &[
    b"...", b"**=", b"<<=", b">>=", b">>>=", b"&&=", b"||=", b"??=", b"=>", b"**", b"+=", b"-=",
    b"*=", b"/=", b"%=", b"&=", b"|=", b"^=", b"=", b"?", b":", b"<",
];

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'\\') || byte >= 0x80
}

struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
    jsx: bool,
    typescript: bool,
    html: bool,
    cap: usize,
    frames: Vec<(Frame, Run)>,
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
    hopeless_regex: usize,
    region_peak: usize,
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

    fn crossed(&mut self, start: usize) -> Option<usize> {
        let depth = self.base + self.run.depth();
        (depth > self.cap * UNITS_PER_LEVEL).then_some(start)
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
        self.frames.push((frame, self.run));
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

    fn region_crossed(&mut self, start: usize) -> Option<usize> {
        let peak = std::mem::take(&mut self.region_peak);
        let depth = self.base + self.run.depth() + peak * UNITS_PER_LEVEL;
        (depth > self.cap * UNITS_PER_LEVEL).then_some(start)
    }

    fn pop(&mut self) -> Frame {
        self.clear_run();
        let (frame, parent) = self.frames.pop().expect("a frame is open");
        self.base -= Self::own(frame) + parent.depth();
        self.run = parent;
        frame
    }

    fn crossing(&mut self) -> Option<usize> {
        if self.bytes.starts_with(b"#!") {
            self.skip_line();
        }

        self.newline = true;
        while self.at < self.bytes.len() {
            let crossed = match self.frames.last() {
                Some((Frame::Template, _)) => self.template_text(),
                Some((Frame::Tag, _)) => self.tag(),
                Some((Frame::Children, _)) => self.children(),
                _ => self.code(),
            };

            if crossed.is_some() {
                return crossed;
            }
        }

        None
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

            let rest = &self.bytes[self.at..];
            let html = self.html
                && (rest.starts_with(b"<!--") || self.newline && rest.starts_with(b"-->"));
            let from = self.at;
            match (self.peek(0), self.peek(1)) {
                (b'/', b'/') => self.skip_line(),
                (b'/', b'*') => self.skip_block_comment(),
                _ if html => self.skip_line(),
                _ => return,
            }

            self.region(from);
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

    fn regex_ends(&mut self) -> bool {
        if self.at < self.hopeless_regex {
            return false;
        }

        let start = self.at;
        let ends = self.skip_regex();
        if !ends {
            self.hopeless_regex = self.at;
        }

        self.at = start;
        ends
    }

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

    fn word(&mut self) -> &'a [u8] {
        let bytes = self.bytes;
        let start = self.at;
        while self.at < bytes.len()
            && is_word_byte(self.peek(0))
            && self.space(self.at) == 0
            && self.line_break(self.at) == 0
        {
            self.at += if self.peek(0) == b'\\' { 2 } else { 1 };
        }

        self.at = self.at.min(bytes.len());
        &bytes[start..self.at]
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
                self.region(start);
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
                        Brace::Block
                    }
                    Prev::Operator if self.after_arrow(start) => Brace::Arrow,
                    _ => Brace::Expression,
                };

                self.prev = if matches!(kind, Brace::Block | Brace::Headed) {
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
                self.run.declaring = false;
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
                self.ended = true;
                None
            }
            b'/' if starts_operand && self.regex_ends() => {
                self.begin(false, false);
                self.skip_regex();
                self.region(start);
                self.prev = Prev::Operand;
                self.region_crossed(start)
            }
            b'<' if self.jsx && starts_operand && self.element_starts() => {
                self.begin(false, false);
                self.at += 1;
                self.push(Frame::Tag, start)
            }
            b'>' if matches!(self.frames.last(), Some((Frame::TypeArguments, _))) => {
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
        let digit_after = rest.get(2).is_some_and(u8::is_ascii_digit);
        let token: &[u8] = OPERATORS
            .iter()
            .find(|&&operator| rest.starts_with(operator) && !(operator == b"?." && digit_after))
            .map_or(&rest[..1], |operator| operator);

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
            }
            _ => {}
        }

        let postfix = self.prev == Prev::Operand && (update || token == b"!" || self.property);
        if closes_angle {
            self.prev = Prev::Operand;
        } else if !postfix {
            self.prev = Prev::Operator;
        }

        let units = if RECURSING_OPERATORS.contains(&token) {
            UNITS_PER_LEVEL
        } else {
            1
        };

        self.count(units, start)
    }

    fn keyword(&mut self, word: &[u8], start: usize, head: bool) -> Option<usize> {
        if word.contains(&b'\\') {
            self.begin(false, false);
            self.prev = Prev::Operand;
            self.expects = true;
            return self.count(UNITS_PER_LEVEL, start);
        }

        let resumes = word == b"else";
        let exported = std::mem::take(&mut self.exported);
        self.begin(resumes || CONTINUING.contains(&word), resumes);

        if TRANSPARENT.contains(&word) {
            self.exported = word == b"export";
            self.divides = word != b"export";
            return None;
        }

        if STATEMENTS.contains(&word) {
            if word == b"default" {
                self.clear_run();
            }

            self.head = word == b"catch";
            self.prev = if exported {
                Prev::Operator
            } else {
                Prev::Statement
            };
            return None;
        }

        if DECLARING.contains(&word) {
            self.run.declaring |= self.prev == Prev::Statement;
            self.divides = !matches!(word, b"function" | b"class" | b"enum");
            self.prev = Prev::Operator;
            return None;
        }

        if word == b"do" {
            self.prev = Prev::Statement;
            self.run.heads += 1;
            return self.crossed(start);
        }

        if HEADS.contains(&word) {
            self.head = true;
            self.prev = Prev::Statement;
            return None;
        }

        if word == b"of" {
            self.prev = Prev::Operator;
            return None;
        }

        if RECURSING.contains(&word) {
            self.head = head && word == b"await";
            self.divides = matches!(word, b"await" | b"yield");
            self.prev = Prev::Operator;
            return self.count(UNITS_PER_LEVEL, start);
        }

        if LIGHT.contains(&word) {
            self.prev = Prev::Operator;
            return self.count(1, start);
        }

        if CONTEXTUAL.contains(&word) {
            let units = if matches!(word, b"as" | b"satisfies") {
                1
            } else {
                UNITS_PER_LEVEL
            };

            self.prev = Prev::Operand;
            self.expects = true;
            return self.count(units, start);
        }

        if OPERANDS.contains(&word) {
            self.prev = Prev::Operand;
            return None;
        }

        if EXPECTING.contains(&word) && self.prev != Prev::Operand {
            if word == b"case" {
                self.clear_run();
            }

            self.prev = Prev::Operator;
            return None;
        }

        self.prev = Prev::Operand;
        None
    }

    fn close(&mut self, byte: u8) {
        let closes = matches!(
            (self.frames.last(), byte),
            (Some((Frame::Paren { .. }, _)), b')')
                | (Some((Frame::Bracket, _)), b']')
                | (Some((Frame::Brace(_) | Frame::Substitution, _)), b'}')
        );

        if !closes {
            return;
        }

        self.prev = match self.pop() {
            Frame::Paren { head: true } => {
                self.head_closed = true;
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
            _ => Prev::Operand,
        };
    }

    fn element_starts(&self) -> bool {
        let rest = &self.bytes[self.at + 1..];
        let Some(&first) = rest.first() else {
            return false;
        };

        if first == b'>' {
            return true;
        }

        if !(first.is_ascii_alphabetic() || matches!(first, b'_' | b'$') || first >= 0x80) {
            return false;
        }

        let from = self.at + 1;
        let name = (from..self.bytes.len())
            .position(|at| {
                !is_word_byte(self.bytes[at]) || self.space(at) > 0 || self.line_break(at) > 0
            })
            .unwrap_or(rest.len());
        let after = &self.bytes[self.skip_trivia(self.at + 1 + name)..];
        let keyword = |word: &[u8]| {
            after.starts_with(word) && !after.get(word.len()).copied().is_some_and(is_word_byte)
        };

        let modifier =
            &rest[..name] == b"const" && after.first().copied().is_some_and(is_word_byte);
        let assigned = after.starts_with(b"=") && !after.starts_with(b"=>");
        !(modifier || keyword(b"extends") || after.starts_with(b",") || assigned)
    }

    fn skip_trivia(&self, mut at: usize) -> usize {
        while at < self.bytes.len() {
            let blank = self.space(at).max(self.line_break(at));
            if blank > 0 {
                at += blank;
            } else if self.bytes[at..].starts_with(b"//") {
                while at < self.bytes.len() && self.line_break(at) == 0 {
                    at += 1;
                }
            } else if self.bytes[at..].starts_with(b"/*") {
                at = self.bytes[at + 2..]
                    .windows(2)
                    .position(|pair| pair == b"*/")
                    .map_or(self.bytes.len(), |end| at + end + 4);
            } else {
                return at;
            }
        }

        at
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
        while self.at < self.bytes.len() {
            let start = self.at;
            match self.peek(0) {
                quote @ (b'"' | b'\'') => {
                    self.skip_quoted(quote, false);
                    self.region(start);
                    if let Some(crossed) = self.region_crossed(start) {
                        return Some(crossed);
                    }
                }
                b'/' if self.peek(1) == b'/' => self.skip_line(),
                b'/' if self.peek(1) == b'*' => self.skip_block_comment(),
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
                    if let Some((frame, _)) = self.frames.last_mut() {
                        *frame = Frame::Children;
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
    Scanner {
        bytes: text.as_bytes(),
        at: 0,
        jsx: source.is_jsx(),
        typescript: source.is_typescript(),
        html: !source.is_module() && !source.is_typescript(),
        cap,
        frames: Vec::new(),
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
        hopeless_regex: 0,
        region_peak: 0,
    }
}

pub fn crossing(text: &str, source: SourceType, cap: usize) -> Option<usize> {
    scanner(text, source, cap).crossing()
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
    fn brackets_in_strings_comments_templates_and_regexes_count_only_inside_them() {
        for line in [
            "x = '[[[';\n",
            "x = \"(((\";\n",
            "// [[[\nx;\n",
            "/* [[[ */ x;\n",
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
            assert!(
                deep(&line.replace("[[[", "[[[[").replace("(((", "((((")),
                "{line:?}"
            );
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
