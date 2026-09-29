function layout(widths: number[]) {
  /* stanza-off */
  const left = widths[0];

  const right = widths[1];

  if (left > right) return "left";

  /* stanza-on */
  const total = left + right;

  return total > 80 ? "wide" : "narrow";
}
