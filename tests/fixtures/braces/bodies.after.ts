function pass(items: Item[], owner: string) {
  for (const item of items) {
    if (item.owner !== owner)
      continue;
    mark(item);
  }

  if (items.length === 0)
    return;
  else
    log(items.length);

  if (owner) {
    if (items[0]) mark(items[0]);
  } else
    reset();

  if (owner) {
    const local = owner.trim();
    use(local);
  }

  if (owner) {
    function inner() {}
  }

  if (owner) {
    let scoped = 1;
  }

  while (items.length > 0) {
    // drop the last one
    items.pop();
  }

  if (owner)
    mark(items[0])

  do
    tick();
  while (items.length > 0);

  if (owner) mark(items[0]);

  if (owner)
    log(owner);
  else if (items.length > 1)
    log(items.length);
}
