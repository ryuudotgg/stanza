---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces stay when a lint config changes a wrapped or tagged value

A lint config that changes its own value through a wrapped receiver, such as `(config as any[]).push(strict)`, `config!.push(strict)` or an alias like `const typed = config as any[]`, or that hands the config to a tag as in ``add`${config}` ``, now keeps braces and prints the unread-config notice. Before, `--fix` read the config's original `curly` setting and could remove braces the config enforces.
