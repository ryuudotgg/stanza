pub fn is_js_whitespace(char: char) -> bool {
    char == '\u{feff}' || (char.is_whitespace() && char != '\u{85}')
}

pub fn is_js_line_terminator(char: char) -> bool {
    matches!(char, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

pub fn is_js_word(unit: impl Into<u32>) -> bool {
    u8::try_from(unit.into()).is_ok_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_what_javascript_trims() {
        assert_eq!(js_trim("\u{feff}\u{a0} x \u{3000}"), "x");
        assert_eq!(js_trim("\u{85}x\u{85}"), "\u{85}x\u{85}");
    }

    #[test]
    fn javascript_character_sets() {
        let table = |char: char| {
            matches!(
                char,
                '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}'
            ) || matches!(char, '\u{2000}'..='\u{200a}')
                || matches!(
                    char,
                    '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
                )
                || char == '\u{feff}'
        };

        let words: Vec<char> = ('A'..='Z')
            .chain('a'..='z')
            .chain('0'..='9')
            .chain(['_'])
            .collect();
        let terminators = ['\n', '\r', '\u{2028}', '\u{2029}'];
        assert!(is_js_whitespace('\u{feff}'));
        assert!(!is_js_whitespace('\u{85}'));
        for char in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
            assert_eq!(is_js_whitespace(char), table(char), "{char:?}");
            assert_eq!(is_js_word(char), words.contains(&char), "{char:?}");
            assert_eq!(
                is_js_line_terminator(char),
                terminators.contains(&char),
                "{char:?}"
            );
        }

        for byte in u8::MIN..=u8::MAX {
            assert_eq!(
                is_js_word(byte),
                words.contains(&char::from(byte)),
                "{byte}"
            );
        }

        for unit in u16::MIN..=u16::MAX {
            let char = char::from_u32(u32::from(unit));
            assert_eq!(
                is_js_word(unit),
                char.is_some_and(|char| words.contains(&char)),
                "{unit}"
            );
        }
    }

    #[test]
    fn counts_utf16_units() {
        assert_eq!(utf16_len("a😀é"), 4);
    }
}
