function ignoredLabels(xs: number[], a: boolean, c: boolean) {
  // stanza-ignore
  label: for (const x of xs) {
    use(x);
  }

  // stanza-ignore
  outer: inner: if (a) {
    b();
  }

  // stanza-ignore
  chain: if (a) {
    b();
  } else if (c) {
    d();
  }

  loose: for (const x of xs) {
    use(x);
  }
}
