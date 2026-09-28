function trailing(x: boolean) {
  if (x) {
    step();
  } // trailing

  next();
}

function eslint(x: boolean) {
  if (x) {
    step();
  } // eslint-disable-line

  next();
}

function directive(x: boolean) {
  if (x) {
    step();
  } // @ts-expect-error

  next();
}

function tight(x: boolean) {
  if (x) {
    step();
  }// tight

  next();
}

function block(x: boolean) {
  if (x) {
    step();
  } /* block */

  next();
}

function separate(x: boolean) {
  if (x)
    step();
  // @ts-expect-error
  next();
}

function branch(x: boolean) {
  if (x)
    step();
  else { // branch
    other();
  }
}
