---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Help says when block-spacing stays quiet

The `block-spacing` summary in `stanza --help` and the hook's block reason now reads `when no rule decides the gap`, since the finding is skipped whenever any rule decides the gap, not only a join rule.
