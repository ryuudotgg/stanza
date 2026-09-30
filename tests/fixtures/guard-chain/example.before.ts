function accepts(entry: Entry, owner: string) {
  if (entry.owner !== owner) return false;

  if (entry.archived) return false;

  if (entry.size > limit) {
    log(entry);
    return false;
  }

  return true;
}
