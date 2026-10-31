use crate::text::utf16_len;

use super::types::{Finding, Rule};

pub fn position(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..text.floor_char_boundary(offset)];
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let line = before.bytes().filter(|&byte| byte == b'\n').count() + 1;
    (line, utf16_len(&before[line_start..]) + 1)
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
}
