use crate::text::{is_js_whitespace, js_trim, utf16_len};

use super::model::{Comment, Doc};
use super::rules::Rule;
use super::types::{Finding, Spot};
use super::widths::WIDTHS;

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

pub fn placed(path: &str, doc: &Doc, spots: Vec<Spot>) -> Vec<Finding> {
    let offsets: Vec<usize> = spots.iter().map(|spot| spot.offset).collect();
    spots
        .into_iter()
        .zip(positions(doc, &offsets))
        .map(|(spot, (line, col))| Finding {
            path: path.to_owned(),
            line,
            col,
            fixable: spot.rule.fixable(),
            rule: spot.rule,
            message: spot.message,
        })
        .collect()
}

fn positions(doc: &Doc, offsets: &[usize]) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..offsets.len()).collect();
    order.sort_by_key(|&index| offsets[index]);

    let mut result = vec![(0, 0); offsets.len()];
    let (mut line, mut at, mut units) = (0, 0, 0);
    for index in order {
        let offset = doc.text.floor_char_boundary(offsets[index]);
        let found = line_at(doc, offset);
        if found != line {
            (line, at, units) = (found, doc.line_starts[found - 1], 0);
        }

        units += utf16_len(&doc.text[at..offset]);
        at = offset;
        result[index] = (line, units + 1);
    }

    result
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

pub fn source<'a>(doc: &Doc<'a>, start: usize, end: usize) -> &'a str {
    &doc.text[start..end]
}

fn width(char: char) -> usize {
    if matches!(char, ' '..='~') {
        return 1;
    }

    table_width(char)
}

fn table_width(char: char) -> usize {
    let point = u32::from(char);
    let index = WIDTHS.partition_point(|&(_, last, _)| last < point);
    match WIDTHS.get(index) {
        Some(&(first, _, width)) if first <= point => usize::from(width),
        _ => 1,
    }
}

pub fn columns(text: &str, tab: usize) -> usize {
    text.chars().fold(0, |count, char| {
        if char == '\t' {
            count.saturating_add(tab - count % tab)
        } else {
            count.saturating_add(width(char))
        }
    })
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
    fn columns_match_bun_string_width() {
        #[derive(serde::Deserialize)]
        struct Sample {
            text: String,
            tab: usize,
            columns: usize,
        }

        let samples: Vec<Sample> =
            serde_json::from_str(include_str!("columns.json")).expect("the sample is JSON");

        for sample in samples {
            assert_eq!(
                columns(&sample.text, sample.tab),
                sample.columns,
                "{:?} at tab {}",
                sample.text,
                sample.tab
            );
        }
    }

    #[test]
    fn ascii_widths_match_the_table() {
        for char in (0..=0x7f_u8).map(char::from) {
            assert_eq!(width(char), table_width(char), "{char:?}");
        }
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
