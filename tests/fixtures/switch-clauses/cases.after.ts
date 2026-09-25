function g(kind: string) {
  switch (kind) {
    case "a":
      return 1;

    case "b":
    case "c":
      return 2;

    // fall through label with comment
    case "d": {
      const x = 3;
      return x;
    }


    default:
      return 0;
  }
}
