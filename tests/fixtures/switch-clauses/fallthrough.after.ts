function g(kind: number) {
  switch (kind) {
    case 1:
      a();
      // falls through

    case 2:
      b();
  }
}
