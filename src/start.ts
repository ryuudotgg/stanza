import { readFileSync } from "node:fs";
import { hookCall } from "./hook.ts";

type Writer = (chunk: string | Uint8Array) => void;

export interface Io {
  cwd: string;
  env: Readonly<Record<string, string | undefined>>;
  stdin: () => Uint8Array;
  stdout: Writer;
  stderr: Writer;
}

function ignoreBrokenPipe(error: NodeJS.ErrnoException): void {
  if (error.code !== "EPIPE") throw error;
}

export function systemIo(): Io {
  // Under Bun a stream write throws EPIPE into a stack trace and exit 1, where console.log swallowed it.
  process.stdout.on("error", ignoreBrokenPipe);
  process.stderr.on("error", ignoreBrokenPipe);

  return {
    cwd: process.cwd(),
    env: process.env,
    stdin: () => readFileSync(0),
    stdout: (chunk) => {
      process.stdout.write(chunk);
    },
    stderr: (chunk) => {
      process.stderr.write(chunk);
    },
  };
}

export async function start(argv: string[]): Promise<number> {
  const io = systemIo();
  if (argv[0] === "hook") {
    const args = argv.slice(1);
    const call = hookCall(args, io);
    if (typeof call === "number") return call;

    const { runHookCall } = await import("./cli.ts");
    return runHookCall(call, args, io);
  }

  const { main } = await import("./cli.ts");
  return main(argv, io);
}
