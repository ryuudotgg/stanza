function totals(values: number[]) {
  let a = 0;
  let b = 0;
  let c = 0;
  // running totals below

  let d = 0;
  let e = 0;
  let f = 0;
  for (const value of values) {
    a += value;
    b += value * 2;
    c = Math.max(c, a + b + d + e + f);
  }

  return { a, b, c, d, e, f };
}
