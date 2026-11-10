---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Fix keeps extended attributes, ACLs and owners

`stanza --fix` and `stanza hook` keep a fixed file's extended attributes and ACLs, and a file owned by another user or group, as when running as root in a container, keeps its owner and is still replaced in one step. A file mounted on its own, as with `docker run -v ./a.ts:/w/a.ts`, is now fixed instead of failing with `EBUSY`. If a write in place fails and the original text cannot be put back, the finding now reads `cannot write: <code>, restore failed, the file may be truncated`.
