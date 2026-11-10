---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Hooks and --changed no longer rewrite .git/index

`stanza hook`, `--check --changed` and `--hunks` used to let git refresh and rewrite `.git/index` whenever a tracked file had a new timestamp but the same content, so a `git add` running at the same moment could fail with `Unable to create index.lock`. Stanza now has git read a private copy of the index, so the repository's index and any submodule's index stay as they were. The selected files are the same as before.
