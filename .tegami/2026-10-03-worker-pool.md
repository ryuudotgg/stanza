---
packages:
  "@ryuugg/stanza": patch
---

### Large CLI runs use worker threads

`--check` and `--fix` format large file selections on worker threads. Output and fixed files stay identical to serial runs. Hooks and small selections stay on the main thread.
