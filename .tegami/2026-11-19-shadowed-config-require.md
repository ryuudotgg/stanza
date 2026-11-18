---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A config's own `require` no longer reads as Node's

When a lint config declares, imports or assigns its own `require`, Stanza no longer treats `require("./base.cjs")` in it as loading that file. It cannot tell what such a call returns, so braces stay and the unread-config notice names the config. Before, `--fix` read the named file and could remove braces the config enforced.
