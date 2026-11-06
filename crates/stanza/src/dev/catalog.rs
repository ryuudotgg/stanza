use serde_json::{Map, Value as Json, json};
use stanza::engine::rules::RULES;
use stanza::hook::CLAUDE_CODE_HOOKS;
use stanza::languages::javascript::entry::ENTRY;
use stanza::usage::{FLAGS, USAGE};

pub fn catalog() -> String {
    let rules: Map<String, Json> = RULES
        .iter()
        .map(|entry| {
            let rule = json!({
                "fixable": entry.fixable,
                "gap": entry.gap,
                "summary": entry.summary,
                "message": entry.message,
            });

            (entry.rule.id().to_owned(), rule)
        })
        .collect();

    let catalog = json!({
        "rules": rules,
        "flags": FLAGS,
        "usage": USAGE,
        "claudeCodeHooks": CLAUDE_CODE_HOOKS,
        "extensions": ENTRY.extensions,
    });

    let text = serde_json::to_string_pretty(&catalog).expect("the catalog serializes");
    format!("{text}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_docs_catalog_matches_the_crate() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/lib/catalog.json");
        let committed = std::fs::read_to_string(path).expect("docs/lib/catalog.json is readable");
        assert!(
            committed == catalog(),
            "docs/lib/catalog.json is stale; regenerate it with `cargo run -q --bin stanza-dev catalog > docs/lib/catalog.json`"
        );
    }
}
