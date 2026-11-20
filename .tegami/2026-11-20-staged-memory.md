---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 `--staged` holds each staged file once

`stanza --check --staged` now reads each staged file straight into its own buffer and hands that buffer to the formatter without copying it, so a large staged change needs about half the memory. Before, it held git's whole reply, a copy of every file cut from it, and another copy of each file while formatting it: 2,000 staged files of 8 KB peaked at 61 MB, and now peak at 28 MB.
