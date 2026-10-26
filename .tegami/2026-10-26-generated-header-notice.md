---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Generated headers explain an empty run

When every file you name is skipped because its header marks it generated, such as `// @generated`, `--check` and `--fix` now print `stanza: nothing to format in <path>, generated files are skipped` on stderr instead of nothing, the same line `.gen.ts` files already get. The exit status is unchanged.
