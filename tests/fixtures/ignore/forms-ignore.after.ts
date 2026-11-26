function f1() {
  first();

  // stanza-ignore
  if (a) {
    run();
  }

  last();
}

function f2() {
  first();

  /* stanza-ignore */
  if (a) {
    run();
  }

  last();
}

function f3() {
  first();

  /** stanza-ignore */
  if (a) {
    run();
  }

  last();
}

function f4() {
  first();

  /**
 * stanza-ignore
 */
  if (a) {
    run();
  }

  last();
}

function f5() {
  first();

  // stanza-ignore because the linter disagrees
  if (a) {
    run();
  }

  last();
}

function f6() {
  first();

  // stanza-ignore: generated table
  if (a) {
    run();
  }

  last();
}
