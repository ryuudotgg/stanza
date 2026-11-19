---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A `.gitignore` git cannot read fails a repository run

Inside a repository, a path run or `--changed` now prints `stanza: cannot read <path>: <reason>` and exits 2 for a `.gitignore` git cannot read, as the walk without git already did. Before, the run went ahead without that file's rules and could check or fix files it meant to skip. This holds in an always skipped folder the `stanza` attribute opts back in too, where `--changed` now also reports a folder git cannot open, as a path run does. A symlinked `.gitignore`, which git does not follow, is still not an error, and the hook still says nothing about an unreadable one.
