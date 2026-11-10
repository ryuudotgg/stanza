---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Runs no longer stall on hosts with broken DNS

On a machine with no `user.email` or `EMAIL` set, a hostname without a dot and an unreachable DNS server, every `stanza` run and hook call used to wait for the resolver to time out, about 10 seconds each. Stanza now asks git for its settings without letting it look up the hostname, so these runs take milliseconds again.
