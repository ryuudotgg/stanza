---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Scoped ESLint configs with a subpath keep their braces

A legacy ESLint `extends` such as `@acme/base/strict` now reads `@acme/eslint-config-base/strict`, the file ESLint loads, instead of the package's main entry. When that file turns `curly` on, `--fix` keeps the braces it used to remove.
