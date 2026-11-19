---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 import.meta and new.target read no binding

A binding named `meta` or `target` no longer joins to a statement that only uses `import.meta` or `new.target`, since neither reads a variable. Before, `const meta = load();` above a blank line and `if (import.meta.url)` got a `guard-join` finding, and `--fix` removed the blank. A statement that reads `meta` or `target` itself still joins.
