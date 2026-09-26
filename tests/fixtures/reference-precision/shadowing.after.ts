function shadowed(other: Target) {
  obj.value = 1;

  if (ready) {
    const obj = other;
    use(obj.value);
  }

  done();
}

function plain() {
  obj.value = 1;
  if (obj.value > 0) run();

  send(obj);
  done();
}

function discriminant(other: Target) {
  obj.value = 1;
  switch (obj.value) {
    case 1:
      const obj = other;
      use(obj);
      break;
  }

  send(obj);
  done();
}

class Holder {
  reset(obj: Target) {
    this.x = 1;
    if (ready) {
      const obj = other;
      use(this.x, obj);
    }

    done();
  }
}
