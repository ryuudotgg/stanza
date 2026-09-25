function load(id: string, input: unknown) {
  const response = fetch(id);
  if (!response.ok) throw new Error(`HTTP ${response.status}`);

  const { user } = parse(input);
  if (!user)
    throw new Error(
      "no user",
    );

  state = next(input);
  if (!state) return;

  const value = compute(id);
  if (value > 3) {
    log(value);
    return value;
  }

  return value;
}
