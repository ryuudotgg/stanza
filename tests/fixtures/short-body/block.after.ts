function report(collected: { errors: string[] }, findings: string[]) {
  if (collected.errors.length > 0) {
    for (const error of collected.errors) console.error(error);
    return 2;
  }

  for (const finding of findings) {
    const line = format(finding);
    console.log(line);
  }

  return findings.length > 0 ? 1 : 0;
}
