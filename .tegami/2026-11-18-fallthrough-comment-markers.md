---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Fall-through comments match ESLint's

`--fix` now keeps a comment with the `case` above it whenever ESLint's `no-fallthrough` would accept it, so `// fall through`, `// fallsthrough` and `// intentionally falls through` get the blank line below them. Before, only a comment starting with `falls through` or `fallthrough` stayed above the next label, and any other marker got the blank line above it. ESLint directives still lead the next `case`.
