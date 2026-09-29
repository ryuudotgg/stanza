function pick(items: Item[], strict: boolean) {
  if (items.length > 1) {
    if (strict) log("many");
  } else {
    log("one");
  }

  while (items.length > 0) {
    // drop the last one
    items.pop();
  }
}
