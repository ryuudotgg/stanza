use crate::text::{is_js_whitespace, js_trim, utf16_len};

use super::model::{Comment, Doc};
use super::rules::Rule;
use super::types::{Finding, Spot};
use super::widths::{BIOME, OXFMT, PICTOGRAPHIC, PRETTIER};

const TERMINAL: &[(char, char, u8)] = BIOME;

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

pub fn placed(doc: &Doc, spots: Vec<Spot>) -> Vec<Finding> {
    let offsets: Vec<usize> = spots.iter().map(|spot| spot.offset).collect();
    spots
        .into_iter()
        .zip(positions(doc, &offsets))
        .map(|(spot, (line, col))| Finding {
            line,
            col,
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

pub fn parse_finding(text: &str, offset: usize, message: String) -> Finding {
    let (line, col) = position(text, offset);
    Finding {
        line,
        col,
        rule: Rule::Parse,
        message,
    }
}

pub fn source<'a>(doc: &Doc<'a>, start: usize, end: usize) -> &'a str {
    &doc.text[start..end]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ruler {
    Prettier,
    Biome,
    Oxfmt,
}

impl Ruler {
    pub fn measure(self, line: &str, tab: usize) -> usize {
        match self {
            Self::Prettier => prettier(line, tab),
            Self::Biome => biome(line, tab),
            Self::Oxfmt => oxfmt(line, tab),
        }
    }
}

fn prettier(line: &str, tab: usize) -> usize {
    let content = line.trim_start_matches([' ', '\t']);
    let indentation = line[..line.len() - content.len()]
        .chars()
        .fold(0, |count: usize, char| {
            count.saturating_add(if char == '\t' { tab } else { 1 })
        });

    content.split('\t').fold(indentation, |count, segment| {
        count.saturating_add(sequenced(segment, PRETTIER))
    })
}

fn biome(line: &str, tab: usize) -> usize {
    line.chars().fold(0, |count: usize, char| {
        count.saturating_add(if char == '\t' {
            tab
        } else {
            cells(BIOME, char)
        })
    })
}

fn oxfmt(line: &str, tab: usize) -> usize {
    let tabs = tab.saturating_mul(line.matches('\t').count());
    line.split('\t').fold(tabs, |count, segment| {
        count.saturating_add(sequenced(segment, OXFMT))
    })
}

pub fn terminal_columns(text: &str) -> usize {
    text.chars().map(|char| cells(TERMINAL, char)).sum()
}

fn cells(table: &[(char, char, u8)], char: char) -> usize {
    if matches!(char, ' '..='~') {
        return 1;
    }

    let index = table.partition_point(|&(_, last, _)| last < char);
    match table.get(index) {
        Some(&(first, _, cells)) if first <= char => usize::from(cells),
        _ => 1,
    }
}

fn sequenced(text: &str, table: &[(char, char, u8)]) -> usize {
    if text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
        return text.len();
    }

    let mut rest = text;
    let mut count: usize = 0;
    while let Some(first) = rest.chars().next() {
        let (counted, taken) = match emoji_sequence(rest) {
            Some(taken) if taken > first.len_utf8() => (2, taken),
            _ => (cells(table, first), first.len_utf8()),
        };

        count = count.saturating_add(counted);
        rest = &rest[taken..];
    }

    count
}

fn emoji_sequence(text: &str) -> Option<usize> {
    keycap(text).or_else(|| joined(text))
}

fn keycap(text: &str) -> Option<usize> {
    let rest = text.strip_prefix(['#', '*', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'])?;
    let rest = rest.strip_prefix('\u{fe0f}').unwrap_or(rest);
    let rest = rest.strip_prefix('\u{20e3}')?;
    Some(text.len() - rest.len())
}

fn joined(text: &str) -> Option<usize> {
    let mut taken = element(text)?;
    while let Some(rest) = text[taken..].strip_prefix('\u{200d}') {
        let Some(next) = element(rest) else {
            break;
        };

        taken += '\u{200d}'.len_utf8() + next;
    }

    Some(taken)
}

fn element(text: &str) -> Option<usize> {
    let first = text.chars().next().filter(|&char| pictographic(char))?;
    let mut rest = &text[first.len_utf8()..];
    rest = rest
        .strip_prefix(|char| matches!(char, '\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}'))
        .unwrap_or(rest);

    let tags = rest.trim_start_matches(|char| matches!(char, '\u{e0020}'..='\u{e007e}'));
    if let Some(cancelled) = tags.strip_prefix('\u{e007f}') {
        rest = cancelled;
    }

    Some(text.len() - rest.len())
}

fn pictographic(char: char) -> bool {
    let index = PICTOGRAPHIC.partition_point(|&(_, last)| last < char);
    PICTOGRAPHIC
        .get(index)
        .is_some_and(|&(first, _)| first <= char)
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
    fn formatter_measure_rows() {
        let rows: [(&str, [usize; 3]); 12] = [
            ("भारत", [4, 4, 4]),
            ("漢字", [4, 4, 4]),
            ("e\u{301}", [1, 1, 1]),
            ("👍🏽", [2, 4, 2]),
            ("👨\u{200d}👩\u{200d}👧", [2, 6, 2]),
            ("❤\u{fe0f}", [2, 1, 2]),
            ("1\u{fe0f}\u{20e3}", [2, 1, 2]),
            ("a\u{200b}b", [3, 2, 2]),
            ("a\u{ad}b", [3, 2, 2]),
            ("กั", [2, 1, 1]),
            ("a\u{85}b", [2, 2, 3]),
            ("ab\tc", [3, 7, 7]),
        ];

        let rulers = [Ruler::Prettier, Ruler::Biome, Ruler::Oxfmt];
        for (text, expected) in rows {
            let measured = rulers.map(|ruler| ruler.measure(text, 4));
            assert_eq!(measured, expected, "{text:?}");
        }

        for ruler in rulers {
            assert_eq!(ruler.measure("\tif (a) b;", 4), 13, "{ruler:?}");
            assert_eq!(ruler.measure("\t\tx", 4), 9, "{ruler:?}");
            assert_eq!(ruler.measure("  \tx", 4), 7, "{ruler:?}");
        }
    }

    #[test]
    fn sequences_match_the_formatters() {
        #[derive(serde::Deserialize)]
        struct Sample {
            text: String,
            prettier: usize,
            biome: usize,
            oxfmt: usize,
        }

        let samples: Vec<Sample> =
            serde_json::from_str(include_str!("sequences.json")).expect("the sample is JSON");

        for sample in samples {
            let measured = [Ruler::Prettier, Ruler::Biome, Ruler::Oxfmt]
                .map(|ruler| ruler.measure(&sample.text, 4));

            assert_eq!(
                measured,
                [sample.prettier, sample.biome, sample.oxfmt],
                "{:?}",
                sample.text
            );
        }
    }

    #[test]
    fn tab_width_saturates() {
        for ruler in [Ruler::Prettier, Ruler::Biome, Ruler::Oxfmt] {
            assert_eq!(ruler.measure("\t\tx", usize::MAX), usize::MAX, "{ruler:?}");
        }
    }

    #[test]
    fn tables_match_the_pinned_formatters() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../../package.json")).expect("package.json");
        let pin = |package: &str| {
            manifest["devDependencies"][package]
                .as_str()
                .unwrap_or_else(|| panic!("{package} has no exact pin"))
                .to_owned()
        };

        let expected = format!(
            "// Generated by bun scripts/width-tables.ts from Prettier {}, Biome {} and oxfmt {}. Do not edit.",
            pin("prettier"),
            pin("@biomejs/biome"),
            pin("oxfmt"),
        );

        assert_eq!(
            include_str!("widths.rs").lines().next(),
            Some(expected.as_str()),
            "a formatter pin moved: run bun scripts/width-tables.ts"
        );
    }

    #[test]
    fn ascii_skips_the_tables() {
        for table in [PRETTIER, BIOME, OXFMT] {
            assert!(
                table
                    .iter()
                    .all(|&(first, last, _)| last < ' ' || first > '~'),
                "a table holds printable ASCII"
            );
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
