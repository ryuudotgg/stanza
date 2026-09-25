function pick(items: Item[], mode: Mode) {
  const first = items[0];
  return first?.name ?? "none";
}

function route(mode: Mode) {
  const kind = classify(mode);
  switch (kind) {
    case "a":
      return 1;

    default:
      return 0;
  }
}

function other(mode: Mode) {
  log(mode);
  log(mode);
  log(mode);
  const kind = classify(mode);

  return mode;
}

function wrap(mode: Mode) {
  log(mode);
  log(mode);
  log(mode);
  const kind = classify(mode);

  return () => use(kind);
}
