---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Stanza, explain and hook name argument mistakes alike

`stanza`, `stanza explain` and `stanza hook` now read their flags the same way, so a mistake gets the same message from each, such as `unknown flag --bogus` or `--no-braces given twice`. Before, explain said `unexpected argument` for an unknown flag, the hook said it for an unknown or repeated flag, and both reported a flag that is not valid UTF-8 as an invalid name. A repeated `--fix` or `--check` now says `--fix given twice` rather than `use one of --fix or --check`, and `stanza hook` accepts `--`.
