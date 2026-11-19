---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Lint configs that set a global stay readable

Stanza now reads `curly` from a lint config that also sets a global to a plain value, such as `process.env.FOO ??= "1"` or `globalThis.DEBUG = true`. Before, `--fix` kept braces and printed the unread-config notice for it. A config that also loads a local file, or stores one of its own values in a global, still keeps braces.
