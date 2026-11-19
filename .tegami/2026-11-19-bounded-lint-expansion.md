---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Long brace patterns in lint overrides keep braces quickly

A lint override whose `files` pattern repeats brace groups thousands of times, such as `{a}{a}{a}...`, used to take seconds and hundreds of megabytes per run, then remove braces the override may enforce. Expanding a pattern now costs time and memory in proportion to its length, and a pattern too large to expand within that bound counts as one Stanza cannot read, so an override that turns `curly` on keeps braces.
