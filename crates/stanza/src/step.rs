use crate::engine::process_file;
use crate::engine::types::{Finding, Mode, Rule};
use crate::files::is_generated_header;

pub type Decoded = Result<String, String>;

const BOM: char = '\u{feff}';

pub fn decode(bytes: Vec<u8>) -> Decoded {
    String::from_utf8(bytes).map_err(|_| "not valid UTF-8, left untouched".to_owned())
}

pub fn without_mark(text: &str) -> &str {
    text.strip_prefix(BOM).unwrap_or(text)
}

#[derive(Default)]
pub struct StepResult {
    pub findings: Vec<Finding>,
    pub parse_error: bool,
    pub generated: bool,
}

pub fn format_text(path: &str, text: Decoded, mode: Mode) -> StepResult {
    let text = match text {
        Ok(text) => text,
        Err(message) => {
            return StepResult {
                findings: vec![Finding {
                    path: path.to_owned(),
                    line: 1,
                    col: 1,
                    rule: Rule::Parse,
                    message,
                    fixable: false,
                }],
                parse_error: true,
                generated: false,
            };
        }
    };

    if is_generated_header(&text) {
        return StepResult {
            generated: true,
            ..StepResult::default()
        };
    }

    let result = process_file(path, without_mark(&text), mode);
    StepResult {
        findings: result.findings,
        parse_error: result.parse_error,
        generated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_order_mark_shifts_no_column() {
        let plain = format_text("a.ts", Ok("let x = ;\n".to_owned()), Mode::Check);
        let marked = format_text("a.ts", Ok("\u{feff}let x = ;\n".to_owned()), Mode::Check);
        assert_eq!(plain.findings, marked.findings);
        assert_eq!((marked.findings[0].line, marked.findings[0].col), (1, 9));
    }

    #[test]
    fn invalid_utf8_is_a_parse_finding() {
        let result = format_text("a.ts", decode(vec![b'a', 0xe9]), Mode::Check);
        assert!(result.parse_error);
        assert_eq!(
            result.findings[0].message,
            "not valid UTF-8, left untouched"
        );
    }
}
