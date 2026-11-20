---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A lint config changed by another module keeps braces

A JavaScript lint config now counts as unreadable when it, or a local module it imports or requires, writes into a module the config loads, as in `require("./base.cjs").overrides.push(strict)` or a bare `require("./tweak")` whose file runs `Object.assign(require("./base").rules, { curly: "error" })`. `--fix` keeps braces and prints the unread-config notice. Before, Stanza read the module as written and could remove braces that the changed `curly` rule enforces. Imports of packages by name, such as `import "dotenv/config"`, still leave the config readable.
