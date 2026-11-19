---
packages:
  "@ryuugg/stanza": patch
---

### ✨ Explain takes `--braces`

`stanza explain` now takes `--braces` as well as `--no-braces`, with the meaning it has for `--check`, so it explains a body the way `stanza --check --braces` judges it instead of saying a lint config keeps the braces. Before, explain rejected `--braces` as an unexpected argument.
