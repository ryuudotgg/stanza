function scan(rows: Row[]) {
  let updated = false;

  for (const row of rows)
    if (row.stale) updated = true;

  let total = 0;

  try {
    total = sum(rows);
  } catch {
    total = -1;
  }

  let count: number;

  if (rows.length > 0) {
    count = rows.length;
    log(count);
  } else {
    count = 0;
    log(count);
  }

  let label = "";

  while (label.length < 3)
    label += "x";

  let raw = read(
    rows,
  );

  if (raw) raw = trim(raw);

  let untouched = 0;

  for (const row of rows) {
    log(row);
    log(rows);
  }

  return { updated, total, count, label, raw, untouched };
}
