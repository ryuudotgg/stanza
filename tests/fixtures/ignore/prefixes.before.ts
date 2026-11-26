/* stanza-off */
if (a) {
  a();
}

// stanza-on-call
if (b) {
  b();
}

// stanza-offline
if (c) {
  c();
}

/* stanza-on */
if (d) {
  d();
}

// stanza-ignored
if (e1) {
  e1();
}

// stanza-off-by-one
if (e2) {
  e2();
}

// stanza-offé
if (e3) {
  e3();
}
