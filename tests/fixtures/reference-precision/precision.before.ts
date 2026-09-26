function longerName() {
  this.x = 1;

  if (this.xy) run();

  log(this.x);
  done();
}

function stringLiteral() {
  a.x = 1;

  if (flag) log("a.x");

  send(a);
  done();
}

function samePath() {
  a.x = 1;

  if (a.x > 0) run();

  send(a);
  done();
}

class Child extends Base {
  reset() {
    super.x = 1;

    if (super.x > 0) run();

    send(this);
    done();
  }
}

function assertedReceiver() {
  (a as Target).x = 1;

  if (a.x > 0) run();

  send(a);
  done();
}

function nestedDeclaration(value: number) {
  const v = value;

  if (ok) {
    function inner() {
      return v;
    }

    run(inner);
  }

  send(v);
  done();
}

function directDeclaration() {
  const v = 1;

  function g() {
    return v;
  }

  run(g);
  done();
}

async function nestedCleanup() {
  setBusy(true);
  try {
    await work();
  } finally {
    later(() => setBusy(false));
  }
}

async function directCleanup() {
  setBusy(true);
  try {
    await work();
  } finally {
    setBusy(false);
  }
}
