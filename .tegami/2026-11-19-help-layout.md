---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Help fits an 80 column terminal

`stanza --help` now fits in 80 columns. Long usage forms, flag descriptions and rule summaries wrap onto indented continuation lines instead of running up to 299 columns, flags sit under a `Flags:` heading with the same indent as the rules, and the Claude Code hook registration prints as indented JSON that pastes into `.claude/settings.json` as is.
