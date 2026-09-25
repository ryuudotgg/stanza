function table(text: string, lines: string[]) {
  const lineStarts: number[] = [];

  let offset = 0;
  for (const line of lines) {
    lineStarts.push(offset);
    offset += line.length + 1;
  }

  const paths: string[] = [];

  let mode: string | undefined;
  for (const arg of lines)
    if (arg === "--fix") mode = arg;
    else paths.push(arg);

  let total = 0;
  for (const line of lines) total += line.length;

  return { lineStarts, paths, mode, total, text };
}
