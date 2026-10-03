// A budget in CPU time holds on a loaded machine, where wall time stretches with
// every other process. A test's wall timeout stays only to stop a hang.
export function cpuMs(): number {
  const { user, system } = process.cpuUsage();
  return (user + system) / 1_000;
}

export function cpuOf(result: { resourceUsage?: { cpuTime: { total: bigint | number } } }): number {
  if (result.resourceUsage === undefined) throw new Error("spawn reported no resource usage");
  return Number(result.resourceUsage.cpuTime.total) / 1_000;
}

export function cpuSpent<T>(body: () => T): { value: T; ms: number } {
  const started = cpuMs();
  const value = body();
  return { value, ms: cpuMs() - started };
}
