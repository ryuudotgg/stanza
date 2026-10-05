---
packages:
  "@ryuugg/stanza": patch
---

### Skip unnecessary Git attribute checks

Path runs, `--changed`, `--stdin <path>`, `explain` and hooks skip the Git attribute subprocess when no attribute source can apply to the files, no `attr.*` config is set, and every `GIT_*` variable set is one Stanza knows cannot affect attribute lookup. Anything else, including `GIT_ATTR_SOURCE` or an attributes path Stanza cannot rebuild, still asks Git, and `--staged` always asks Git.
