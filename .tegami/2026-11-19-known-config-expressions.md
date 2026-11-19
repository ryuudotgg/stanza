---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Lint configs using ?. and || are read

A lint config that spreads `...(js.configs.recommended?.rules || {})` now reads like the same config without `?.` and `||`, so `--fix` removes braces it permits and prints no unread-config notice. Optional member reads such as `a?.b` and `a?.["b"]` follow known objects and read as undefined past `null` or `undefined`, and `a || b` picks a side only when Stanza knows whether `a` is truthy. Before, either expression kept braces with the notice.
