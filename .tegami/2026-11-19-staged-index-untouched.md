---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 --staged no longer rewrites .git/index

`--check --staged` used to let git rewrite `.git/index` when a staged file had a newer timestamp, or when a filtered file's staged attributes differ from the work tree's and Stanza writes a tree to read them, so a `git add` or `git commit` running at the same moment could fail with `Unable to create index.lock`. Git now works on private copies of the index for both steps, as `--changed` already did, and the repository's index stays as it was. The files checked and the findings are the same as before.
