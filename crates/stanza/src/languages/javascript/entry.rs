use std::io;

use oxc_span::GetSpan;

use crate::engine::explain::{Opening, Verdict};
use crate::engine::model::{BracePass, Doc, Rejection};
use crate::languages::{
    BracesSetting, ConfigHold, Entry, Examined, Language, Layout, WidthFrom, WidthSetting,
};

use super::braces::{Controlled, brace_edits, openings, verdict};
use super::config::coverage::{Setting, Target};
use super::config::width::{WidthSource, formatter_width};
use super::config::{brace_decisions, braces_setting};
use super::facts::Node;
use super::scan::{examine, examine_source};

const EXTENSIONS: [&str; 8] = [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];

fn generated_name(name: &str) -> bool {
    name.contains(".generated.")
        || EXTENSIONS.iter().any(|extension| {
            name.strip_suffix(extension).is_some_and(|stem| {
                stem.ends_with(".gen") || (extension.ends_with("js") && stem.ends_with(".min"))
            })
        })
}

fn skips_name(name: &str, keep_generated: bool) -> bool {
    name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
        || (!keep_generated && generated_name(name))
}

pub(crate) struct JavaScript;

impl Language for JavaScript {
    const EXTENSIONS: &'static [&'static str] = &EXTENSIONS;
    const DIRECTORIES: &'static [&'static str] = &[
        "node_modules",
        "dist",
        "build",
        ".git",
        ".next",
        "out",
        "coverage",
        "migrations",
        "drizzle",
    ];

    type Node<'a> = Node<'a>;
    type Body<'a> = Controlled<'a>;

    fn skips_name(name: &str, keep_generated: bool) -> bool {
        skips_name(name, keep_generated)
    }

    fn anchor(node: &Node) -> usize {
        node.anchor()
    }

    fn body_span(body: &Controlled) -> (usize, usize) {
        let span = body.block.span();
        (span.start as usize, span.end as usize)
    }

    fn body_owner(body: &Controlled) -> usize {
        body.owner
    }

    fn examine<R>(
        path: &str,
        text: &str,
        layout: Layout,
        then: impl FnOnce(&Doc, &Examined<'_, Self>, Option<String>) -> R,
    ) -> R {
        examine(path, text, layout, then)
    }

    fn examine_source<R>(
        path: &str,
        text: &str,
        layout: Layout,
        then: impl FnOnce(&Doc, &Examined<'_, Self>) -> R,
    ) -> Result<R, Rejection> {
        examine_source(path, text, layout, then)
    }

    fn brace_pass(doc: &Doc, bodies: &[Controlled], layout: Layout) -> BracePass {
        brace_edits(doc, bodies, layout)
    }

    fn openings(doc: &Doc, bodies: &[Controlled], line: usize) -> Vec<Opening> {
        openings(doc, bodies, line)
    }

    fn verdict(
        doc: &Doc,
        eligible: &[Controlled],
        offset: usize,
        at: &dyn Fn(usize) -> usize,
    ) -> Verdict {
        verdict(doc, eligible, offset, at)
    }

    fn width(path: &str) -> WidthSetting {
        let width = formatter_width(path);
        let layout = width.layout();
        let source = match width.source {
            WidthSource::Config(file) => WidthFrom::Config(file),
            WidthSource::Default(formatter) => WidthFrom::Default(formatter.name()),
            WidthSource::Fallback => WidthFrom::Fallback,
        };

        WidthSetting {
            layout,
            columns_text: width.columns_text,
            source,
            unread: width.unread,
        }
    }

    fn braces(path: &str) -> BracesSetting {
        braces_setting(path)
    }

    fn brace_reasons(path: &str) -> io::Result<Vec<ConfigHold>> {
        let decisions = brace_decisions(&Target::file(path))?;
        let holds = decisions
            .into_iter()
            .filter(|decision| decision.setting != Setting::Off)
            .map(|decision| ConfigHold {
                enforced: decision.setting == Setting::On,
                files: decision.files,
            })
            .collect();

        Ok(holds)
    }
}

pub static ENTRY: Entry = Entry::of::<JavaScript>();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_name_matches_registered_suffixes() {
        let cases = [
            ("a.gen.ts", true),
            ("a.gen.jsx", true),
            ("b.min.js", true),
            ("b.min.mjs", true),
            ("b.min.cjs", true),
            ("x.generated.ts", true),
            ("regen.ts", false),
            ("admin.mjs", false),
            ("min.js", false),
            ("gen.cjs", false),
            ("b.min.ts", false),
            ("b.min.jsx", false),
            ("bundle.min.ts", false),
        ];

        for (name, generated) in cases {
            assert_eq!(generated_name(name), generated, "{name}");
        }
    }

    #[test]
    fn declaration_files_stay_skipped_when_keeping_generated() {
        for name in ["a.d.ts", "a.d.mts", "a.d.cts"] {
            assert!(skips_name(name, true), "{name}");
        }

        assert!(!skips_name("a.gen.ts", true));
    }
}
