function leading(a: boolean, x: number, f: (x: number) => number, g: unknown) {
  if (a)
    x = f(x)

  ;(g as any)()

  if (a)
    x = f(x);

  ;(g as any)()
}
