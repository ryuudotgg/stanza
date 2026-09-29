function load(id: string, cache: Map<string, Record>) {
  const record = cache.get(id);

  if (!record) {
    log(`miss ${id}`);
    return null;
  }

  return record;
}
