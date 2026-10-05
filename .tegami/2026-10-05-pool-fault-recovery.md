---
packages:
  "@ryuugg/stanza": patch
---

### Lost worker threads are reported and their files still formatted

When a run that uses worker threads loses one, because it fails to start or dies after taking files, Stanza reports the loss with a `stanza:` line on stderr and exits 2, and still prints the same findings and writes the same files as a serial run. Before, a worker that failed to start went unreported and a worker that died after taking files hung the run. Runs that start no worker thread, such as a single file or `STANZA_WORKERS=0`, no longer load the worker machinery, so they start slightly faster.
