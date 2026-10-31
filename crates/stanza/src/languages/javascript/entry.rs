pub struct Entry {
    pub extensions: &'static [&'static str],
    pub directories: &'static [&'static str],
    pub skips_name: fn(&str, bool) -> bool,
}

const EXTENSIONS: [&str; 8] = [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];

fn generated_name(name: &str) -> bool {
    name.contains(".generated.")
        || EXTENSIONS.iter().any(|extension| {
            name.ends_with(&format!(".gen{extension}"))
                || (extension.ends_with("js") && name.ends_with(&format!(".min{extension}")))
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
