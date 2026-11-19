---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 An empty run gives each path its own reason

When `--check` or `--fix` finds nothing to format, each path now gets the reason that applies to it, so `stanza --check a.gen.ts ignored` says that `a.gen.ts` is generated and that git ignores `ignored`, where it used to give both one reason. A git repository inside the path that the outer repository does not track is now named on its own line, saying to pass its path, and no longer makes the outer files read as ignored by git.
