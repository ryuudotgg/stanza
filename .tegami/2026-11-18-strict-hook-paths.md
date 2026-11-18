---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The hook no longer fixes a file the agent never named

`stanza hook` now treats a string holding an unpaired surrogate escape such as `\ud800` as naming no file, whether it is a tool path in its input, a write in a transcript or a change in a Codex rollout, and a transcript or rollout line that is not valid UTF-8 names no file either, without making Stop fall back to every changed file. A `cwd`, `transcript_path`, `agent_transcript_path` or `hook_event_name` holding such an escape, and hook input that is not valid UTF-8, now exit 1 with a notice such as `input must be valid UTF-8`. Before, each of these was decoded with a replacement character, so the hook could rewrite an unrelated file literally named `�.ts`.
