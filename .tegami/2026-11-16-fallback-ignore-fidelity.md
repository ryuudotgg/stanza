---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The walk without git ignores what git ignores

Outside a git work tree, or with git missing from `PATH`, the directory walk now reads a `.gitignore` only when it is a regular file. A symlinked one is skipped as git skips it, so its rules no longer hide files git would list, and a link to a FIFO or device, or a FIFO itself, no longer hangs the run. A `.gitignore` that is a socket or a link to a folder used to stop the run with `cannot read .gitignore` and exit 2; it is now skipped. The walk also strips a byte order mark at the start of a `.gitignore`, treats a pattern holding an unknown class such as `[[:foo:]]` as matching nothing, lets `**\/` cross folders and matches the first character of a reversed range such as `[z-a]`, all as `git check-ignore` does.
