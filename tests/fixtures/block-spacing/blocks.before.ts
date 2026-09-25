function run(id: string, items: Item[]) {
  const item = find(id);
  if (!item) return;
  if (item.owner !== id) {
    log(item);
    return;
  }

  setBusy(true);
  try {
    work(items);
    work(items);
  } finally {
    setBusy(false);
  }

  const logger = getLogger();
  for (const entry of items) {
    count += entry.size;
    log(count);
  }

  if (id) out.a = item.a;
  if (id) out.b = item.b;
  if (id) {
    out.c = item.c;
    out.d = item.d;
  }

  return item;
}
