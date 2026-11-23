---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 max_line_length = off means no limit under Prettier

Under Prettier, an `.editorconfig` that sets `max_line_length = off` now gives no width limit, as Prettier reads it, so `stanza --fix` joins every guard whose braces come off and `stanza explain` prints the width as `unlimited`. Before, Stanza used Prettier's default 80 and kept longer guards split. Under oxfmt, `off` still gives oxfmt's default 100.
