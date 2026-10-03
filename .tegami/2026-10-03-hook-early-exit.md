---
packages:
  "@ryuugg/stanza": patch
---

### The hook returns at once when there is nothing to format

`stanza hook` now answers an event it ignores, or an edit that touched no TypeScript or JavaScript file such as `package.json`, without running git or loading the formatter. Those calls now take about as long as `stanza --version`.
