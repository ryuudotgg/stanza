---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Plain-name and suffix rules in `.gitignore` no longer slow selection outside git

Outside git, a `.gitignore` rule that is a plain name such as `dist` or a suffix such as `*.log` is checked without building any matching state, so a long list of them costs little more than no `.gitignore` at all. Other wildcard rules and bracket sets are cheaper too. Selecting 5,000 files under 600 plain-name rules drops from about 13.8 billion instructions to about 1.7 billion, against 1.55 billion with no `.gitignore`. The same files are selected.
