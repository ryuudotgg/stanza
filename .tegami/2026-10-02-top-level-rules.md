---
packages:
  "@ryuugg/stanza": minor
---

### Blank line rules apply at module top level

The blank line rules now cover the gaps between top level statements, not just the ones inside blocks. A `let` that the function below it reads joins that function and gets a blank line above it, a multi-line statement gets a blank line after it, and a run of six statements is reported as a wall. `export const` and `export function` count as the declaration they export. Imports and re-exports stay one group, with no blank line forced between them and no wall reported for them. [short-body](https://stanza.ryuu.gg/rules/short-body) skips the top level, so a short file loses a blank line only when another rule asks for it.
