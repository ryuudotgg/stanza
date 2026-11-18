---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 EditorConfig widths and tabs read the way the formatter reads them

`stanza --fix` now takes `max_line_length`, `tab_width` and `indent_size` from `.editorconfig` only where Prettier or oxfmt would take the same value. Under Prettier a value must be a JSON number that is a positive whole number, so `0x78`, `+120`, `0120` and `120 # note` no longer set a width, and a `.editorconfig` with a line Prettier cannot parse, such as a bare word or an unclosed `[*.ts`, sets nothing. Under oxfmt and Biome a value must be a whole decimal number and keys are case sensitive, so `1.2e2`, `120.0` and `MAX_LINE_LENGTH` no longer count, and when oxfmt would reject the width or tab it takes, Stanza names the `.editorconfig` in the unread notice instead of using it. A width or tab written before the first section no longer counts, and `indent_style = space` with an `indent_size` sets the tab before `tab_width` does, as the formatters do.
