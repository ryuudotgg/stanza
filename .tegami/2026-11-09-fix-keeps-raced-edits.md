---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Fix keeps edits made while it runs

`stanza --fix` and `stanza hook` no longer replace a file that changed after stanza read it, such as one an editor saved mid-run. The file keeps its new text and is reported as `write changed while stanza ran, left untouched`: `--fix` exits 2, and the Stop hook lists the file in its block reason, so the next run fixes it.
