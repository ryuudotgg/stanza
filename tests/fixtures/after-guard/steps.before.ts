function directoryFiles(dir: string, parseError: boolean, findings: string[]) {
  const root = repository(dir);
  if (!root) return fallbackFiles(dir);
  const result = runGit(dir, ["ls-files"]);
  if (!result.ok) throw new Error("git failed");
  if (result.output === "") return [];
  log(result);
  if (parseError) return 2;
  return findings.length > 0 ? 1 : 0;
}

function keepsIfBelow(item: Item | null, id: string) {
  const owner = find(id);
  if (!item) return;
  if (item.owner !== id) {
    log(item);
    return;
  }
  use(owner);
}
