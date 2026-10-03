export async function start(addon: string): Promise<void> {
  // Must precede any oxc-parser import (its loader cannot find the binding in a compiled binary), hence the dynamic import.
  process.env.NAPI_RS_NATIVE_LIBRARY_PATH = addon;
  const { start } = await import("./start.ts");
  process.exitCode = await start(process.argv.slice(2));
}
