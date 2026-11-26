@dec(() => {
  // stanza-off
  a();
}) export class A {}

function f() {
  const x = 1;

  if (x) return;
}
