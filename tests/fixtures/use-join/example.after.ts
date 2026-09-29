function readSettings(path: string) {
  const fallback = defaults();
  try {
    return parse(readFileSync(path, "utf8"));
  } catch {
    return fallback;
  }
}
