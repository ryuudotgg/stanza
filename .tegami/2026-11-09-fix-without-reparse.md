---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Blank line fixes finish sooner

`--fix` no longer parses a file a second time when the fix only adds or removes blank lines, so fixing a repository full of spacing findings spends about a quarter less time computing. Fixes that also add or remove braces, and `--hunks` runs, still check the fixed text the way they did before.
