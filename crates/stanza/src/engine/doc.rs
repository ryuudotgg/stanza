use crate::text::{is_js_whitespace, js_trim, utf16_len};

use super::model::{Comment, Doc};
use super::types::{Finding, Rule};

pub fn document<'a>(text: &'a str, comments: Vec<Comment<'a>>) -> Doc<'a> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut line_starts = Vec::with_capacity(lines.len());

    let mut offset = 0;
    for line in &lines {
        line_starts.push(offset);
        offset += line.len() + 1;
    }

    Doc {
        text,
        lines,
        line_starts,
        comments,
    }
}

pub fn line_at(doc: &Doc, offset: usize) -> usize {
    doc.line_starts.partition_point(|&start| start <= offset)
}

pub fn position(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..text.floor_char_boundary(offset)];
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let line = before.bytes().filter(|&byte| byte == b'\n').count() + 1;
    (line, utf16_len(&before[line_start..]) + 1)
}

pub fn finding(path: &str, doc: &Doc, offset: usize, rule: Rule, message: String) -> Finding {
    let line = line_at(doc, offset);
    Finding {
        path: path.to_owned(),
        line,
        col: utf16_len(&doc.text[doc.line_starts[line - 1]..offset]) + 1,
        rule,
        message,
        fixable: rule.fixable(),
    }
}

pub fn parse_finding(path: &str, text: &str, offset: usize, message: String) -> Finding {
    let (line, col) = position(text, offset);
    Finding {
        path: path.to_owned(),
        line,
        col,
        rule: Rule::Parse,
        message,
        fixable: false,
    }
}

pub fn blank_lines(doc: &Doc, after: usize, before: usize) -> impl Iterator<Item = usize> {
    (after + 1..before).filter(|&line| {
        doc.lines
            .get(line - 1)
            .is_some_and(|text| js_trim(text).is_empty())
    })
}

pub fn comment_index(doc: &Doc, offset: usize) -> usize {
    doc.comments
        .partition_point(|comment| comment.start < offset)
}

pub fn next_token(doc: &Doc, offset: usize) -> usize {
    let mut cursor = offset;
    while let Some(char) = doc.text[cursor..].chars().next() {
        if is_js_whitespace(char) {
            cursor += char.len_utf8();
            continue;
        }

        match doc.comments.get(comment_index(doc, cursor)) {
            Some(comment) if comment.start == cursor => cursor = comment.end,
            _ => break,
        }
    }

    cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_utf16_units_past_astral_characters() {
        let text = "a\n😀😀 x\n";
        assert_eq!(position(text, text.find('x').unwrap()), (2, 6));
        assert_eq!(position(text, 0), (1, 1));
        assert_eq!(position(text, text.len()), (3, 1));
    }

    #[test]
    fn lines_split_on_line_feed_only() {
        let doc = document("a\r\n\n b\n", Vec::new());
        assert_eq!(doc.line_starts, [0, 3, 4, 7]);
        assert_eq!(
            (line_at(&doc, 0), line_at(&doc, 3), line_at(&doc, 6)),
            (1, 2, 3)
        );
        assert_eq!(blank_lines(&doc, 0, 4).collect::<Vec<_>>(), [2]);
    }
}
