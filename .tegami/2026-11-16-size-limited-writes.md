---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A write past the file size limit is reported instead of killing stanza

Under a file size limit such as `ulimit -f`, a `--fix` write that would pass the limit used to kill `stanza` with no output, which could leave a `.stanza-*.tmp` file behind or a hard linked file cut short. Now the file gets a `write` finding saying `cannot write: file too large`, the run exits 2 and the original bytes stay, or the finding adds that the file may be truncated when they could not be restored.
