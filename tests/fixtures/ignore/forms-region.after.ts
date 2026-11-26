// stanza-off
if (a1) {
  a1();
}
// stanza-on

if (b1) b1();

/* stanza-off */
if (a2) {
  a2();
}
/* stanza-on */

if (b2) b2();

/** stanza-off */
if (a3) {
  a3();
}
/** stanza-on */

if (b3) b3();

/**
 * stanza-off
 */
if (a4) {
  a4();
}
/**
 * stanza-on
 */

if (b4) b4();

// stanza-off because the linter disagrees
if (a5) {
  a5();
}
// stanza-on because the linter disagrees

if (b5) b5();

// stanza-off: generated table
if (a6) {
  a6();
}
// stanza-on: generated table

if (b6) b6();
