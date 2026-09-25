function c(text: string) {
  const parsed = JSON.parse(
    text,
  );

  // explain the guard
  if (!parsed) return null;

  const a = 1; // trailing
  /* block
     comment */
  return a;
}
