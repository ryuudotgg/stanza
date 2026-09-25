function h(items: string[]) {

  for (const item of items) {

    if (item) {
      use(item);
      log(item);

    }
    count += 1;

  }

}

function k() {

  // leading
  a();
  b();
  // dangling

}
