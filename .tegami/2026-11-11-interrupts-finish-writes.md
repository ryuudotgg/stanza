---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Interrupting a fix leaves no temp file behind

A Ctrl-C, a cancelled CI job or a closed terminal during `stanza --fix` or `stanza hook` could leave a `.stanza-<pid>-<hex>.tmp` file next to a source file, which `git add -A` then picked up, and an interrupted write in place could leave a file empty. Stanza now finishes the file it is writing, starts no other, and then stops on SIGINT, SIGTERM or SIGHUP as before, so every file is either untouched or fully fixed. A signal that was ignored when stanza started, as under `nohup`, stays ignored.
