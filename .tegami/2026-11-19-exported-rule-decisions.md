---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Lint configs are judged by the rules they export

Stanza now decides whether braces are enforced only from the rule positions ESLint or oxlint reads in the exported config. A `curly` or `style` key anywhere else, such as an unused binding, a spread key the config replaces, `settings` or a rule option, no longer makes `--fix` keep braces and print the unread-config notice.

### 🐛 A `__proto__` key in a lint config keeps braces

A JavaScript lint config whose object sets `__proto__`, such as `{ __proto__: strict }`, now counts as unreadable, so `--fix` keeps braces and prints the unread-config notice. Before, Stanza read past the prototype and could remove braces that the inherited `curly` rule enforces.
