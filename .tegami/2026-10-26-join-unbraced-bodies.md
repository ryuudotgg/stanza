---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 A body that fits goes onto its header line when its braces go

When the `braces` rule removes the braces around a body, `--fix` now prints the body on its header line, as `if (done) return;`, when that line fits the formatter width, the way oxfmt and Biome print it. Before, the body stayed on its own line, so the formatter's check failed until it joined the line itself, and the next stanza run changed the blank lines around it again. A body that does not fit, a header that spans lines, or a line comment before the brace keeps the body on its own line.
