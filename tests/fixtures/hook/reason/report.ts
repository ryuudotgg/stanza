export function report(rows: Row[]) {
  const a = rows[0];
  const b = rows[1];
  const c = rows[2];
  const d = rows[3];
  const e = rows[4];
  const f = rows[5];
  const g = rows[6];
  const h = rows[7];
  for (const row of rows) {
    print(row);
    flush(row);
  }

  return [a, b, c, d, e, f, g, h];
}
