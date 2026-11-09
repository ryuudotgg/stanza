---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Hook events on tracked files skip the untracked file listing

When every file an edit or a turn's transcript names is already tracked, `stanza hook` reads the git index to see that and no longer starts `git ls-files --others`, which can only list untracked files. A `PostToolUse` or a `Stop` with a transcript now starts one git process fewer, which matters most in large repositories. New and untracked files are still found the same way, and the files picked stay the same.
