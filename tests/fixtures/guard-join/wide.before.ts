function run(argv: string[], low: number, high: number, offset: number) {
  const args = parseArguments(argv);

  if (!args) {
    console.error(usage);
    return 2;
  }

  const middle = Math.floor((low + high) / 2);

  if (lineStarts[middle] <= offset) low = middle;
  else high = middle;

  const lines: string[] = [];

  for (const line of argv) {
    lines.push(line);
    log(line);
  }

  const unrelated = 1;

  for (const line of argv) {
    log(line);
    log(argv);
  }

  return lines.length + unrelated;
}
