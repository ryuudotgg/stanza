---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Explain quotes each source line once

`stanza explain` quotes a source line once per answer, even when several gaps, braced bodies or walls on that line would each quote it. A minified line that opens 1,600 bodies now prints its source once rather than 1,600 times, so the answer stays a few hundred kilobytes instead of 18 MB. Every section still prints its own rule and result.
