function f(a: number, text: string) {
  const raw = JSON.parse(
    text,
  );

  if (!raw) throw new Error("x");

  const out = {
    a,
  } as const;

  expect(out)
    .toEqual({ a });

  expect(raw)
    .toBeTruthy();

  return out;
}
