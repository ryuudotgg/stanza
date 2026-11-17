---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Deeply nested package manifests no longer crash a run

`stanza --check` and `--fix` finish beside a `package.json` nested more than 127 levels deep, whether it is the project's own or one a lint config imports from `node_modules`, where they used to abort with a stack overflow. Such a manifest counts as unread, as does one holding an out of range number or a lone surrogate escape, so braces stay and the usual notice names the config.
