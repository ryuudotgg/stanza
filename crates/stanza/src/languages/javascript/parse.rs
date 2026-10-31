use oxc_allocator::Allocator;
use oxc_ast::ast::TemplateElement;
use oxc_ast_visit::Visit;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{SourceType, Span};

use crate::paths::extname;

pub struct Rejection {
    pub start: usize,
    pub message: String,
}

pub fn source_type(path: &str) -> SourceType {
    let jsx = SourceType::unambiguous().with_jsx(true);
    match extname(path) {
        ".js" => jsx,
        ".mjs" => jsx.with_module(true),
        ".cjs" => jsx.with_commonjs(true),
        _ => SourceType::from_path(path).unwrap_or_default(),
    }
}

#[derive(Default)]
struct Quasis(Vec<Span>);

impl<'a> Visit<'a> for Quasis {
    fn visit_template_element(&mut self, element: &TemplateElement<'a>) {
        self.0.push(element.span);
    }
}

fn lone_crs(text: &str) -> impl Iterator<Item = usize> {
    let bytes = text.as_bytes();
    (0..bytes.len())
        .filter(move |&index| bytes[index] == b'\r' && bytes.get(index + 1) != Some(&b'\n'))
}

pub fn rejection(path: &str, text: &str) -> Option<Rejection> {
    let allocator = Allocator::default();
    let options = ParseOptions {
        enable_ident_hashes: false,
        ..ParseOptions::default()
    };

    let parsed = Parser::new(&allocator, text, source_type(path))
        .with_options(options)
        .parse();

    if let Some(error) = parsed.diagnostics.first() {
        return Some(Rejection {
            start: error
                .labels
                .first()
                .map_or(0, |label| label.offset() as usize),
            message: error.message.to_string(),
        });
    }

    let mut crs = lone_crs(text).peekable();
    crs.peek()?;

    let mut quasis = Quasis::default();
    quasis.visit_program(&parsed.program);
    quasis.0.sort_by_key(|span| span.start);

    let mut index = 0;
    for offset in crs {
        while index < quasis.0.len() && quasis.0[index].end as usize <= offset {
            index += 1;
        }

        if index == quasis.0.len() || offset < quasis.0[index].start as usize {
            return Some(Rejection {
                start: offset,
                message: "CR line endings are not supported, left untouched".to_owned(),
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_type_per_extension() {
        let cases = [
            ("a.ts", true, false, None),
            ("a.tsx", true, true, None),
            ("a.mts", true, false, Some(true)),
            ("a.cts", true, false, Some(false)),
            ("a.js", false, true, None),
            ("a.jsx", false, true, None),
            ("a.mjs", false, true, Some(true)),
            ("a.cjs", false, true, Some(false)),
        ];

        for (path, typescript, jsx, module) in cases {
            let source = source_type(path);
            assert_eq!(source.is_typescript(), typescript, "{path} typescript");
            assert_eq!(source.is_jsx(), jsx, "{path} jsx");
            match module {
                Some(true) => assert!(source.is_module(), "{path} module"),
                Some(false) => assert!(source.is_commonjs(), "{path} commonjs"),
                None => assert!(source.is_unambiguous(), "{path} unambiguous"),
            }
        }
    }

    #[test]
    fn lone_cr_inside_a_template_is_kept() {
        assert!(rejection("a.ts", "const t = `a\rb`;\n").is_none());
        let outside = rejection("a.ts", "a();\rb();\n").expect("a lone CR is rejected");
        assert_eq!(outside.start, 4);
    }

    #[test]
    fn first_error_label_starts_the_rejection() {
        let rejected = rejection("a.ts", "let x = ;\n").expect("a syntax error is rejected");
        assert_eq!(rejected.start, 8);
    }
}
