---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 TOML and EditorConfig lines end where Prettier ends them

A U+2028 or U+2029 inside a comment or quoted value no longer leaves `.prettierrc.toml` unread, and an `.editorconfig` with lone CR line ends now gives Prettier projects its width. A `.prettierrc.toml` holding a lone CR that Prettier rejects is left unread.
