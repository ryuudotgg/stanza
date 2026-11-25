---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Negative numbers and empty keys no longer hide a config

A negative number such as `-1`, or a property named `""`, no longer stops Stanza from reading a Prettier config in `package.json`, `.prettierrc.json`, `.prettierrc.json5` or a JavaScript file, so `--check`, `--fix` and `explain` use a valid line width it sets instead of printing "cannot read the line width" and falling back. ESLint and Oxlint configs with either now decide whether braces are enforced, where Stanza used to keep the braces because it could not tell.
