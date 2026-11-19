---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The hook formats a Write for the file the agent writes

A relative `Write` path under a symlinked `cwd` is now joined to `cwd` as Claude Code and Codex join it, so `../a.ts` names the `a.ts` beside the link that the agent writes, with that file's settings, `--hunks` check and opt-out. The hook used to step up from the folder the link points to and format the content for another file. `Edit`, `MultiEdit` and `apply_patch` paths already worked this way and keep doing so.
