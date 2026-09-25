function loop(x: boolean, g: () => void) {
  do {
    if (x) {
      g();
    }
  } while (x);
}
