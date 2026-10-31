use std::cmp::Ordering;
use std::sync::LazyLock;

use icu_collator::options::CollatorOptions;
use icu_collator::{Collator, CollatorBorrowed};

pub fn is_js_whitespace(char: char) -> bool {
    matches!(
        char,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}'
    ) || matches!(char, '\u{2000}'..='\u{200a}')
        || matches!(
            char,
            '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
        )
        || char == '\u{feff}'
}

pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

pub fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

static COLLATOR: LazyLock<CollatorBorrowed<'static>> = LazyLock::new(|| {
    Collator::try_new(Default::default(), CollatorOptions::default())
        .expect("the root collation data is compiled in")
});

pub fn locale_compare(left: &str, right: &str) -> Ordering {
    COLLATOR.compare(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_what_javascript_trims() {
        assert_eq!(js_trim("\u{feff}\u{a0} x \u{3000}"), "x");
        assert_eq!(js_trim("\u{85}x\u{85}"), "\u{85}x\u{85}");
    }

    #[test]
    fn counts_utf16_units() {
        assert_eq!(utf16_len("a😀é"), 4);
    }

    #[test]
    fn collates_like_bun() {
        let mut words = [
            "z", "B", "a.ts", "é", "a/b", "b", "A.ts", "a.b", "a-b", "a_b",
        ];
        words.sort_by(|left, right| locale_compare(left, right));
        assert_eq!(
            words,
            [
                "a_b", "a-b", "a.b", "a.ts", "A.ts", "a/b", "b", "B", "é", "z"
            ]
        );
    }
}
