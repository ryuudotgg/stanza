function parseConfig(text: string) {
  const raw = JSON.parse(
    text,
  );

  if (!isRecord(raw)) throw new Error("config must be an object");
  return raw;
}
