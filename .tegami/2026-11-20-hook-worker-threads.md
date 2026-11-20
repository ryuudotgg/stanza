---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 The hook formats many files on worker threads

`stanza hook` now spreads a large selection over worker threads under the same `STANZA_WORKERS` rule as `stanza --fix --changed`, so a Stop over hundreds of changed files finishes sooner. Before, every hook run formatted its files one at a time and ignored `STANZA_WORKERS`.
