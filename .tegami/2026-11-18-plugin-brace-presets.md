---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces stay under a lint plugin preset that enforces them

`--fix` reads a plugin preset such as `plugin.configs.strict` in an oxlint or flat ESLint config, or `plugin:local/strict` in `.eslintrc`, from the plugin package and keeps braces when the preset turns `curly` on. Before, Stanza assumed every plugin preset left `curly` alone and removed the braces. A preset that turns `curly` off still lets Stanza remove braces, and a preset whose plugin is not installed or cannot be read without running it keeps braces, with the unread-config notice when no config turns braces on.
