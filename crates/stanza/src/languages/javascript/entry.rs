pub struct Entry {
    pub extensions: &'static [&'static str],
    pub directories: &'static [&'static str],
    pub skips_name: fn(&str, bool) -> bool,
}

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

pub static ENTRY: Entry = Entry {
    extensions: &EXTENSIONS,
    directories: &[
        "node_modules",
        "dist",
        "build",
        ".git",
        ".next",
        "out",
        "coverage",
        "migrations",
        "drizzle",
    ],
    skips_name,
};

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
