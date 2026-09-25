function crlf(text: string) {
  const raw = JSON.parse(
    text,
  );
  if (!raw) throw new Error("x");
  log(raw);
  log(text);
  return raw;
}
