function render(rows: Row[]) {
  const header = buildHeader(rows);

  // stanza-ignore: keep the header on its own step
  if (rows.length === 0) return header;

  const body = rows.map(renderRow);
  return [header, ...body].join("\n");
}
