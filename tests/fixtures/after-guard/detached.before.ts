function detached(get: () => Item | null) {
  const x = get();

  // why

  if (!x) return null;
  log(x);
  log(x);
  return x;
}
