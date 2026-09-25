function fix(text: string, edits: number[]) {
  const start = text.length;

  let current = text;
  let count = 0;
  let offset = start;
  for (const edit of edits) {
    current = current.slice(0, edit);
    count++;
    offset += edit;
  }

  return { current, count, offset };
}

function sum(edits: number[]) {
  let unused = 0;

  let total = 0;
  for (const edit of edits) total += edit;

  return { unused, total };
}
