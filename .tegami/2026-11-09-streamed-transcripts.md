---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Stop hook reads long transcripts in far less memory

The Stop and SubagentStop hooks now read the session transcript and each subagent transcript one line at a time, so a 366 MB transcript peaks at about 9 MB instead of over 700 MB. The files they pick and what they print stay the same.
