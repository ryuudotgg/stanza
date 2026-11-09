---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Long lines with many findings report faster

Columns are now worked out in one pass over each line, so a file whose findings crowd onto one long line, such as bundled or minified code, no longer slows down with every finding added. A one line file with 4000 findings drops from about a third of a second to a few milliseconds in `--check`, and every finding keeps the line and column it had before.
