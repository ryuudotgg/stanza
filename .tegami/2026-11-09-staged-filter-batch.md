---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 `--staged` reads filtered files through one git process

When staged files have a clean or smudge `filter`, `--staged` now reads them all through one `git cat-file --batch --filters` instead of one `git cat-file --filters` per file, so five filtered files start one git process where they used to start five. Files whose driver runs a `filter.<driver>.process`, and paths holding a newline, ending in a carriage return or starting with a space or tab, are still read one at a time. Which files are checked and what is printed are unchanged.
