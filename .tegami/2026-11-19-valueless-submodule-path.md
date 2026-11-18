---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A submodule path key with no value names no submodule

A directory run or `--changed` no longer reports a skipped submodule for a `.gitmodules` entry whose `path` key has no value. Before, such a key was read as its own path, so a gitlink at a folder spelled like the key, such as `submodule.a.path`, was named as a skipped submodule. Entries with a real `path = <dir>` are still named.
