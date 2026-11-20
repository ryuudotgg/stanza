---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 oxfmt and Biome widths above 320 are unread

An oxfmt or Biome config that sets a line width above 320, at the top level, in a language section or in any override, now counts as unreadable, as those formatters refuse it: `--fix` prints the unread-config notice and takes the width from the next source. Before, Stanza used that width even though the formatter refuses to run with the config.
