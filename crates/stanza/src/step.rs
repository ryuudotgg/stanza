use crate::engine::types::{Braces, FileResult, Mode};
use crate::files::Changed;
use crate::languages::{BraceRule, Settings, source_entry};

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
    let entry = source_entry(path);
    let width = entry.width(path);
    unread.width.extend(width.unread.iter().cloned());
    let mut braces = entry.braces(path, braces);
    if let BraceRule::Config(setting) = &mut braces {
        unread.braces.append(&mut setting.unread);
    }

    let settings = Settings { width, braces };
    let mut result = entry.format(path, body, mode, &settings, lines);
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

    #[test]
    fn a_path_outside_the_registry_formats_as_javascript() {
        let text = "function f() {\n  a();\n\n\n  b();\n}\n";
        let fixed = |path: &str| {
            format_text(
                path,
                text,
                Mode::Fix,
                Some(Braces::Off),
                &mut Unread::default(),
                None,
            )
            .fixed
            .map(|fixed| fixed.text)
        };

        assert_eq!(fixed("case.TS"), fixed("case.ts"));
        assert_eq!(fixed("scratch"), fixed("case.ts"));
        assert_eq!(
            fixed("case.ts").as_deref(),
            Some("function f() {\n  a();\n  b();\n}\n")
        );
    }
}
