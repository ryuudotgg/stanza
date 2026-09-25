function tpl(use: (s: string) => void) {
  const s = `a
b`; use(s);
  return s;
}
