function musl(): boolean {
  if (process.platform !== "linux") return false;
  const report = process.report?.getReport() as { header?: { glibcVersionRuntime?: string } };
  return !report?.header?.glibcVersionRuntime;
}

export function hostPlatform(): string {
  return `${process.platform}-${process.arch}${musl() ? "-musl" : ""}`;
}
