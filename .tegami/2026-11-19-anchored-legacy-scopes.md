---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A leading `./` anchors oxlint and legacy ESLint overrides

An override pattern that starts with `./` in an oxlint config or a legacy ESLint config now matches only from the config's own directory, as oxlint and ESLint read it. Before, `./*.ts` also reached `src/a.ts`, so an override that turns `curly` off there let `--fix` remove braces the root config enforces.
