---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Legacy ESLint `extends` finds the file Node loads

An extensionless path in a legacy ESLint `extends`, or in a `require` call inside a JavaScript config, now resolves in Node's CommonJS order: the exact file, then `.js`, `.json` and `.node`, then the directory's `package.json` `main` and its `index` files. With `base.js` beside `base.json`, `"extends": ["./base"]` used to read `base.json`, so `--fix` could remove braces that the `curly` rule in `base.js` enforces. A `.node` file it reaches counts as unreadable, so braces stay.
