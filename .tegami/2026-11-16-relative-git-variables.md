---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Relative git repository variables point where git points

A relative `GIT_WORK_TREE`, `GIT_COMMON_DIR` or `GIT_OBJECT_DIRECTORY` now resolves against the folder stanza starts in, as git does, even when `GIT_DIR` is not set. Naming a file in a subfolder keeps the repository root, so its `.gitattributes` still applies. Before, a relative `GIT_WORK_TREE` made the file's own folder the work tree and checked files marked `linguist-generated`, and a relative `GIT_COMMON_DIR` or `GIT_OBJECT_DIRECTORY` failed with exit 2.
