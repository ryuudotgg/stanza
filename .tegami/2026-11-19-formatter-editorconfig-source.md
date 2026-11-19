---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Each formatter's `.editorconfig` is read the way it reads it

Under Biome, `stanza --fix` no longer takes the line width from `max_line_length` or the tab width from `tab_width`: it takes only `indent_size`, as Biome does, so a guard Biome would split stays split, and an `.editorconfig` Biome would refuse, such as one with an `indent_size` above 24 in any section, is reported as unread. Under oxfmt it reads the one `.editorconfig` beside the oxfmt config, or beside the `package.json` that lists oxfmt, which is the file oxfmt reads when run from that directory, rather than the one nearest the file. Under Prettier, a regular file or a link named `.hg` no longer stops the search for `.editorconfig`.
