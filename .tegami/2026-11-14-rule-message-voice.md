---
packages:
  "@ryuugg/stanza": minor
---

### 🚨 Findings say what to change

Every rule's finding message now starts with the change to make, such as `add a blank line after the multi-line statement above` or `remove the braces around this single statement body`, and a wall names its length, as in `split these 8 statements into steps with a blank line`. The new words show in the finding rows, in `--json` and in the hook's block reason, so a script that matches the old messages must change.
