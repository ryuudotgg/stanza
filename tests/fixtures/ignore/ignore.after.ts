function ignoredStatement(value: string) {
  const frozen = value;
  // stanza-ignore: preserve this step
  if (!frozen) return;

  use(frozen);

  /* stanza-ignore */
  if (value)
    keep(value);
  if (value)
    remove(value);
}

function ignoredEdge() {
  // stanza-ignore
  start();
  finish();
}

function ignoredRegion(value: string) {
  before(value);

  /* stanza-off */
  if (value) {
    function nested() {
      const selected = value;
      if (selected)
        use(selected);
    }

    nested();
  }

  const held = value;
  if (held)
    keep(held);

  /* stanza-on */
  const after = value;
  if (after)
    fixed(after);
}

function unmatchedRegion(value: string) {
  if (value) {
    /* stanza-off */
    const raw = value;
    if (raw)
      use(raw);
  }

  const afterBlock = value;
  if (afterBlock)
    fixed(afterBlock);
}

function ignoredChain(value: number) {
  // stanza-ignore
  if (value > 1)
    big(value);
  else if (value > 0)
    small(value);
  else
    none();

  if (value)
    done(value);
}

function cancelled(value: string) {
  // stanza-ignore

  if (value)
    gone(value);
}

function trailingOff(value: string) {
  use(value);
  /* stanza-off */
}

function ignoredBlock(value: string) {
  if (value)
    // stanza-ignore
      kept(value);
}
