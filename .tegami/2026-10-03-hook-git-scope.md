---
packages:
  "@ryuugg/stanza": patch
---

### The hook asks git only about the files you wrote

`stanza hook` asks git only about the files an edit or a Stop transcript names, rather than walking the whole worktree, so hooks stay fast in large repos. Results are unchanged.
