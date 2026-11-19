---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A git failure in one repository no longer stops the hook in the others

When the agent wrote files in several repositories and git failed in one of them, such as on a corrupt index, `stanza hook` used to fix nothing at all. It now still fixes and reports the files in the other repositories, writes the failure on stderr and exits 1, so Claude Code and Codex show a notice beside the usual block decision or read-again context.
