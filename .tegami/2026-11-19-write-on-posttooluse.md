---
packages:
  "@ryuugg/stanza": minor
---

### 🚨 Claude Code Writes are fixed after they land

`stanza hook` now fixes a Write on PostToolUse, as it does an Edit and MultiEdit, so register it once with the matcher `Write|Edit|MultiEdit` and drop the PreToolUse entry for Write. A PreToolUse event now prints nothing, so a registration copied from an earlier release leaves each Write unformatted until the Stop pass: replace it with the one in the README. The permission prompt and the Write result show the text as the agent sent it, and the agent is told to read the file again when Stanza changed it. Under `--hunks`, a Write to a file tracked in HEAD now has its changed lines fixed instead of waiting for the Stop pass.
