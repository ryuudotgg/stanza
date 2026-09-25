function asi(x: boolean, g: () => void) {
  if (x) {
    return {}
  }

  (g)();

  if (x) {
    log(x)
  }

  (g)();

  if (x)
    log(x)

  g();

  if (x) {
    if (x) {}
  }

  [g][0]();

  if (x)
    g();

  g();

  if (x) {
    log(x)
  }

  <number>g;
}
