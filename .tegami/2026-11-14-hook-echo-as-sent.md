---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 PreToolUse returns the Write input as it was sent

When `stanza hook` formats a Write, `updatedInput` is now the tool input exactly as Claude Code sent it, with only the last `content` value replaced. Repeated keys, key order, escapes, whitespace and numbers come back as written, where before they were rewritten the way JavaScript's `JSON.stringify` writes them. A tool input nested 40,000 levels deep is now formatted instead of failing the hook. A Write whose content holds a lone surrogate, such as an unpaired `\ud800` escape, is left alone, and a lone surrogate in `file_path` reads as U+FFFD in the message that asks the agent to read the file again.
