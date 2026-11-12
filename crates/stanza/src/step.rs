use crate::engine::process_file;
use crate::engine::rules::Rule;
use crate::engine::types::{Braces, Finding, Fixed, Mode};
use crate::files::{Changed, is_generated_header};
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

#[derive(Default)]
pub struct StepResult {
    pub findings: Vec<Finding>,
    pub fixed: Option<String>,
    pub parse_error: bool,
    pub generated: bool,
}

fn failure(rule: Rule, message: String) -> StepResult {
    StepResult {
        findings: vec![Finding {
            line: 1,
            col: 1,
            rule,
            message,
            fixable: false,
        }],
        parse_error: true,
        ..StepResult::default()
    }
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
    text: &Decoded,
    mode: Mode,
    braces: Option<Braces>,
    unread: &mut Unread,
    lines: Option<Changed>,
) -> StepResult {
    let text = match text {
        Ok(text) => text,
        Err(message) => return failure(Rule::Parse, message.clone()),
    };

    if is_generated_header(text) {
        return StepResult {
            generated: true,
            ..StepResult::default()
        };
    }

    let body = without_mark(text);
    let mark = &text[..text.len() - body.len()];
    let width = formatter_width(path);
    unread.width.extend(width.unread.iter().cloned());
    let keep = keep_braces(path, braces, &mut unread.braces);
    let result = process_file(path, body, mode, keep, width.layout(), lines);
    let fixed = match result.fixed {
        Some(Fixed {
            error: Some(error), ..
        }) => {
            let message = format!("fixed text does not parse ({error}), left untouched");
            return failure(Rule::Error, message);
        }
        Some(Fixed { text, error: None }) => Some(format!("{mark}{text}")),
        None => None,
    };

    StepResult {
        findings: result.findings,
        fixed,
        parse_error: result.parse_error,
        generated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_order_mark_shifts_no_column() {
        let plain = format_text(
            "a.ts",
            &Ok("let x = ;\n".to_owned()),
            Mode::Check,
            Some(Braces::Off),
            &mut Unread::default(),
            None,
        );
        let marked = format_text(
            "a.ts",
            &Ok("\u{feff}let x = ;\n".to_owned()),
            Mode::Check,
            Some(Braces::Off),
            &mut Unread::default(),
            None,
        );
        assert_eq!(plain.findings, marked.findings);
        assert_eq!((marked.findings[0].line, marked.findings[0].col), (1, 9));
    }

    #[test]
    fn invalid_utf8_is_a_parse_finding() {
        let result = format_text(
            "a.ts",
            &decode(vec![b'a', 0xe9]),
            Mode::Fix,
            Some(Braces::Off),
            &mut Unread::default(),
            None,
        );
        assert!(result.parse_error);
        assert_eq!(
            result.findings[0].message,
            "not valid UTF-8, left untouched"
        );
    }
}
