---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Stop hooks skip malformed tool calls in a transcript

When an agent transcript holds a tool call whose name or id is not a string, `stanza hook` on Stop and SubagentStop now skips that call and still limits the check to the files the rest of the transcript wrote. Before, such a name was converted to a string the way JavaScript does, a call with a number id or no id still counted, and a name that was an object with its own `toString` made the hook fail.
