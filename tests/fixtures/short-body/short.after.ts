function two(a: number) {
  const b = double(a);
  log(b);
}

const three = (a: number) => {
  const b = double(a);
  const c = triple(a);
  return b + c;
};

class K {
  method() {
    // set up
    prepare();
    finish();
  }
}

function four(a: number) {
  const b = double(a);

  const c = triple(a);

  const d = b + c;
  return d;
}

function notShort(a: number) {
  const b = double(a);
  if (b > 2) {
    log(b);
    log(a);
  }
}

function guarded(name: string) {
  const value = flag(name);
  if (value === undefined)
    throw new Error(name);
  return value;
}
