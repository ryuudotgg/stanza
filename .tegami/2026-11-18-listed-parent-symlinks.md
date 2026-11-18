---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A tracked folder replaced by a symlink no longer leads `--fix` into its target

When a tracked folder such as `src` was removed and replaced by a link to another folder in the repository, `stanza --check .`, `--changed` and the hook still took the files git remembered under `src` and checked or rewrote them in the link's target, even when git ignores that target. Folder arguments and `--changed` now skip a file git lists when a folder on its path below the repository root, or below the folder you named, is a symlink. Naming a symlinked folder, as in `stanza --fix alias`, still formats the files in its target.
