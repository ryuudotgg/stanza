---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces stay when a lint config changes itself through a local name

A lint config that changes its own value through another name now keeps braces and prints the unread-config notice, wherever that name is bound: a `const` or `let` inside a block or a function, a destructured binding or its default, a plain assignment, a `for of` loop variable such as `for (const list of [config])`, or a parameter of a callback given to a method, as in `[config].forEach((list) => list.push(strict))` or `[config].forEach(add)`. Before, `--fix` read the config's original `curly` setting and could remove braces the config enforces. Names are matched by scope, so a config whose code fills an unrelated local value, or reuses a parameter name in another callback, still reads as before.
