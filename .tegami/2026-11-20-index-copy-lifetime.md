---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A stopped `--changed` run leaves no index copy behind

`--changed` and the agent hooks now delete their private copy of the git index as soon as they have chosen the files, before formatting starts. Before, the copy stayed in `TMPDIR` until the run ended, so a run stopped while formatting or printing left a `stanza-index-*` folder behind, one the size of the repository's index.
