import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";

export const root = join(import.meta.dirname, "..");

export const platforms = readdirSync(join(root, "npm"), { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
  .sort();

export function packageDirectory(platform: string, base = root): string {
  return join(base, "npm", platform);
}

export function stage(dist: string, base = root): void {
  const missing = platforms.filter((platform) => !existsSync(join(dist, `stanza-${platform}`)));
  if (missing.length)
    throw new Error(
      missing.map((platform) => `missing ${join(dist, `stanza-${platform}`)}`).join("\n"),
    );

  for (const platform of platforms) {
    const directory = packageDirectory(platform, base);
    const binary = join(directory, "stanza");
    copyFileSync(join(dist, `stanza-${platform}`), binary);
    chmodSync(binary, 0o755);
    copyFileSync(join(base, "LICENSE.md"), join(directory, "LICENSE.md"));
  }
}

export function tarballName(platform?: string): string {
  return platform ? `stanza-${platform}.tgz` : "stanza.tgz";
}

export function pack(out: string, base = root): string[] {
  const unstaged = platforms.filter(
    (platform) => !existsSync(join(packageDirectory(platform, base), "stanza")),
  );

  if (unstaged.length)
    throw new Error(
      unstaged.map((platform) => `npm/${platform} holds no binary: run stage first`).join("\n"),
    );

  mkdirSync(out, { recursive: true });

  const packed: string[] = [];
  for (const platform of [undefined, ...platforms]) {
    const tarball = join(out, tarballName(platform));
    const cwd = platform ? packageDirectory(platform, base) : base;
    const result = spawnSync("bun", ["pm", "pack", "--quiet", "--filename", tarball], {
      cwd,
      encoding: "utf8",
    });

    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`cannot pack ${cwd}: ${result.stderr.trim()}`);

    packed.push(tarball);
  }

  return packed;
}

if (import.meta.main) {
  const [command, dir, ...rest] = process.argv.slice(2);
  try {
    if (!dir || rest.length || (command !== "stage" && command !== "pack"))
      throw new Error("usage: bun scripts/packages.ts stage <dist> | pack <out>");

    if (command === "stage") stage(resolve(dir));
    else for (const tarball of pack(resolve(dir))) console.log(tarball);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(
      message
        .split("\n")
        .map((line) => `packages: ${line}\n`)
        .join(""),
    );

    process.exitCode = message.startsWith("usage:") ? 2 : 1;
  }
}
