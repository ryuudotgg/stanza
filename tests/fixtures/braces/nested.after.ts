function nest(rows: Row[]) {
  let updated = false;
  for (const row of rows)
    if (row.stale)
      updated = true;
  return updated;
}
