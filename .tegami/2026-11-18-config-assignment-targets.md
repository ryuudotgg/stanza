---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces stay when a lint config is reassigned through a pattern

A lint config that writes its rules through a destructuring assignment, such as `[config.rules.curly] = ["error"]`, or through a `for in` or `for of` head, such as `for (config.rules of [strict.rules]);`, now reads as unknown. `--fix` keeps braces and prints the unread-config notice. Before, Stanza trusted the value the config was declared with and could remove braces the linter enforces.
