---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Tab indented bodies join their header when braces come off

When `--fix` removes braces from a body that fits, it now joins the body onto its header line with one space whatever whitespace sat between them. Before, a body indented with tabs stayed on its own line unless a space sat between the header and its brace, so a brace on a line of its own, a brace after a tab, or `){` kept the body apart while the same code indented with spaces joined.
