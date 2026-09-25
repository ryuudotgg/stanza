function t(stream: Stream, cmd: Cmd) {
  const piped = stream.pipe(
    Effect.ignore,
  ).pipe(Effect.ignore);

  const key = lookup(
    cmd,
  )?.commandKey;

  const list = [
    key,
  ];

  use(piped, key, list);
  return list;
}
