function lineStarts(lines: string[]) {
  const starts: number[] = [];

  let offset = 0;
  for (const line of lines) {
    starts.push(offset);
    offset += line.length + 1;
  }

  return starts;
}
