---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Unknown override options no longer hide a config's width

A Prettier or oxfmt JavaScript config keeps its width when an override holds a value Stanza cannot evaluate but that value cannot change the file's width or tab, such as `plugins: [process.env.PLUGIN]` or an override for `*.svelte` while formatting a `.ts` file. `--fix` then joins what fits and prints no unread-config notice. The config stays unread when the unknown value sets the file's width or tab, or decides which files an override covers. A config that reads a top-level `const`, `let` or class before it is initialized, such as `const o = { a: o }`, now counts as unread, since Prettier fails to load it. Before, any unknown override value discarded the whole config, and such a config was read as valid.
