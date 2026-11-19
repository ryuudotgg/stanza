---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 EditorConfig sections apply to the files the formatter applies them to

Under Prettier, an `.editorconfig` section now sets the width and tab width for exactly the files Prettier applies it to: `[a\ b.ts]` and `[a\*.ts]` read their escapes, `[{a}.ts]` matches only `{a}.ts`, `[{1..3}*.ts]` matches `12.ts`, and `[src[!x]1/b.ts]` no longer matches `src/1/b.ts`. Under oxfmt, which matches sections its own way, a section that oxfmt and EditorConfig answer differently for a file, such as `[a.ts]` or `[{a,b}.ts]` for `src/a.ts`, makes Stanza report that `.editorconfig` as unread and take the next width source instead of guessing. A section pattern Stanza cannot match exactly, such as an extglob, a POSIX class or a numeric range of more than 256 values, now gets the same notice when the section sets a width or tab key.
