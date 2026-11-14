---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Stanza finishes runs under memory limits and from a deleted directory

Under an address space limit such as `ulimit -v 900000`, `--version`, `--help` and usage errors used to exit 2 without running, and a run whose worker threads could not start exited 2 with a warning per thread. Those commands now run as usual, the run's main thread steps down to a smaller stack when it has to, and worker threads that cannot start leave one warning without changing the exit code. Started from a deleted directory, stanza used to panic with exit 101; it now formats absolute paths as usual and prints `stanza: cannot read the working directory: no such file or directory` with exit 2 for anything that needs that directory, or exit 1 from `stanza hook` when its input has no absolute `cwd`. Any other internal error outside a file now prints one `stanza: internal error:` line and exits 2, or 1 from the hook, in place of a Rust panic.
