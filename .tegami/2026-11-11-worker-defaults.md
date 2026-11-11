---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Runs use more worker threads on machines with more cores

Without `STANZA_WORKERS`, a run of 64 files or more now uses one worker thread fewer than the available parallelism, up to eight, where before it waited for 256 files and used at most three. On an 18 core machine a 1454 file `--check` finishes about a fifth sooner, and every finding and fix is unchanged. `STANZA_WORKERS` keeps its range and still overrides the default at any file count.
