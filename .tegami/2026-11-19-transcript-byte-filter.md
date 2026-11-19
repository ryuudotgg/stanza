---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Escaped transcript keys count on Stop

A Stop pass now reads keys and values a transcript or rollout spells with unicode escapes, so a failed Edit whose result spells `is_error` that way no longer counts as a write, and an escaped `tool_use` or `item_completed` now names its file.

### 🐛 Stop reads a transcript in the format it starts in

Stanza reads the file as a Claude Code transcript or a Codex rollout from its first record that identifies one, so a Codex record late in a Claude Code transcript no longer makes Stop read it as a rollout, and a rollout that holds only `session_meta` changes no file instead of falling back to every changed file in the repository.

### 🎨 Long transcripts cost the Stop pass less

The Stop pass passes over transcript lines that cannot name a write on their bytes alone, without decoding them, so a long transcript takes about half the instructions it did.
