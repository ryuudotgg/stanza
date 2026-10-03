---
packages:
  "@ryuugg/stanza": patch
---

### Joined guards and declarations no longer report unavoidable walls

`wall` no longer reports groups of six or more statements that join rules keep together. Shorter joined groups still count toward clearable walls, and separate walls in the same body still report. Join rules and fixed output stay unchanged.
