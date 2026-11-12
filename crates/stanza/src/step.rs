use crate::engine::process_file;
use crate::engine::types::{Braces, FileResult, Mode};
use crate::files::Changed;
use crate::languages::javascript::config::braces_setting;
use crate::languages::javascript::config::width::formatter_width;
use crate::paths::{dirname, extname};

pub type Decoded = Result<String, String>;

const BOM: char = '\u{feff}';

pub fn decode(bytes: Vec<u8>) -> Decoded {
    String::from_utf8(bytes).map_err(|_| "not valid UTF-8, left untouched".to_owned())
}

pub fn without_mark(text: &str) -> &str {
    text.strip_prefix(BOM).unwrap_or(text)
}

#[derive(Default)]
pub struct Unread {
    pub braces: Vec<String>,
    pub width: Vec<String>,
}

pub fn keep_braces(path: &str, braces: Option<Braces>, unread: &mut Vec<String>) -> bool {
    if let Some(braces) = braces {
        return braces == Braces::Off;
    }

    let setting = braces_setting(&dirname(path), Some(extname(path)));
    unread.extend(setting.unread);
    setting.enforced
}

pub fn format_text(
    path: &str,
    text: &str,
    mode: Mode,
    braces: Option<Braces>,
    unread: &mut Unread,
    lines: Option<Changed>,
) -> FileResult {
    let body = without_mark(text);
    let mark = &text[..text.len() - body.len()];
    let width = formatter_width(path);
    unread.width.extend(width.unread.iter().cloned());
    let keep = keep_braces(path, braces, &mut unread.braces);
    let mut result = process_file(path, body, mode, keep, width.layout(), lines);
    if let Some(fixed) = &mut result.fixed
        && !mark.is_empty()
    {
        fixed.text.insert_str(0, mark);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_order_mark_shifts_no_column() {
        let plain = format_text(
            "a.ts",
            "let x = ;\n",
            Mode::Check,
            Some(Braces::Off),
            &mut Unread::default(),
            None,
        );
        let marked = format_text(
            "a.ts",
            "\u{feff}let x = ;\n",
            Mode::Check,
            Some(Braces::Off),
            &mut Unread::default(),
            None,
        );
        assert_eq!(plain.findings, marked.findings);
        assert_eq!((marked.findings[0].line, marked.findings[0].col), (1, 9));
    }
}
