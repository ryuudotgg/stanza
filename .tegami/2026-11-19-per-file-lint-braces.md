---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Lint overrides scoped to file names decide braces per file

The braces rule now reads each file against the lint overrides whose patterns match that file's own path. An override that turns `curly` or `useBlockStatements` on for `**/*.config.ts` keeps braces in `src/vite.config.ts` and lets `--fix` remove them from `src/a.ts`, and an override that turns the rule off for `**/*.test.ts` removes braces only in test files. Exclusions through ESLint `ignores` or `excludedFiles`, oxlint `excludeFiles`, and `!` patterns in Biome `includes` now leave out exactly the files they name when Stanza can match the pattern exactly, and a pattern it cannot match still keeps braces. Before, a file-name pattern counted for every file of its extension in the folder, so braces stayed everywhere.
