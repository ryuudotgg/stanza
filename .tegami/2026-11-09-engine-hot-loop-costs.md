---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Long `else if` chains and long `let` runs check faster

Stanza does less work per statement, so a whole repository check runs about 5% fewer instructions. An unbraced `else if` chain, and a long run of `let` declarations read by the statement after it, now take time that grows with their length rather than its square: 8000 of either goes from one to three and a half billion instructions down to about a tenth of a billion. What Stanza reports and fixes stays the same.
