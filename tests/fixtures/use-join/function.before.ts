function fallbackFiles(dir: string) {
  const patterns = readPatterns(dir);
  const files: string[] = [];

  function walk(current: string): void {
    for (const entry of readdirSync(current)) {
      if (ignored(entry, patterns)) continue;
      files.push(entry);
    }
  }

  walk(dir);
  return files;
}
