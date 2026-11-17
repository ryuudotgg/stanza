---
packages:
  "@ryuugg/stanza": minor
---

### 🚨 Finding rows separate their fields with colons

`--check` and `--fix` now print each finding as `path:line:col: rule: message`, and a finding `--fix` leaves for you ends with ` (by hand)`. A path holding a control character, a line or paragraph separator, `"` or `\` prints quoted, in the escapes `git status` uses, such as `"a\nb.ts"`, so every finding stays on one line and no file name can restyle the terminal. `--fix --stdin` prints the same rows on stderr, and `--json` is unchanged, so a script that parses the old rows must change, or read `--json` instead.
