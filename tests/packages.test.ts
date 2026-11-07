import { expect, test } from "bun:test";
import { cpSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pack, packageDirectory, platforms, stage, tarballName } from "../scripts/packages.ts";
import { scratch } from "./support.ts";

const root = join(import.meta.dir, "..");
const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));

function entries(tarball: string): string[] {
  const listed = Bun.spawnSync(["tar", "-tzf", tarball]);
  expect(listed.exitCode, listed.stderr.toString()).toBe(0);
  return listed.stdout.toString().trim().split("\n").sort();
}

function packed(tarball: string, file: string): string {
  return Bun.spawnSync(["tar", "-xOzf", tarball, `package/${file}`]).stdout.toString();
}

test("every release platform has a package matching its os, cpu and libc", () => {
  expect(platforms).toEqual([
    "darwin-arm64",
    "darwin-x64",
    "linux-arm64",
    "linux-arm64-musl",
    "linux-x64",
    "linux-x64-musl",
  ]);

  for (const platform of platforms) {
    const [os, cpu, libc] = platform.split("-");
    const pkg = JSON.parse(readFileSync(join(packageDirectory(platform), "package.json"), "utf8"));

    expect(pkg.name).toBe(`@ryuugg/stanza-${platform}`);
    expect(pkg.version).toBe(manifest.version);
    expect(pkg.bin).toBeUndefined();
    expect(pkg.preferUnplugged).toBe(true);

    expect(pkg.os).toEqual([os]);
    expect(pkg.cpu).toEqual([cpu]);
    expect(pkg.libc).toEqual(os === "linux" ? [libc ?? "glibc"] : undefined);
  }
});

test("the npm package depends on exactly the platform packages at its own version", () => {
  expect(manifest.optionalDependencies).toEqual(
    Object.fromEntries(
      platforms.map((platform) => [`@ryuugg/stanza-${platform}`, manifest.version]),
    ),
  );

  expect(manifest.dependencies).toBeUndefined();
  expect(manifest.engines).toEqual({ node: ">=20.10" });
});

test("the npm package packs only the launcher and each platform package only its binary", () => {
  const base = scratch("packages");
  for (const file of ["package.json", "README.md", "LICENSE.md", "src/launcher"])
    cpSync(join(root, file), join(base, file));

  writeFileSync(join(base, "src", "cli.ts"), "export {};\n");

  const dist = join(base, "dist");
  mkdirSync(dist);

  for (const platform of platforms) {
    mkdirSync(packageDirectory(platform, base), { recursive: true });
    cpSync(
      join(packageDirectory(platform), "package.json"),
      join(packageDirectory(platform, base), "package.json"),
    );

    writeFileSync(join(dist, `stanza-${platform}`), `binary for ${platform}`);
  }

  stage(dist, base);
  const tarballs = pack(join(base, "out"), base);

  expect(tarballs).toEqual(
    [undefined, ...platforms].map((platform) => join(base, "out", tarballName(platform))),
  );

  expect(entries(tarballs[0]!)).toEqual([
    "package/LICENSE.md",
    "package/README.md",
    "package/package.json",
    "package/src/launcher",
  ]);

  for (const [index, platform] of platforms.entries()) {
    const tarball = tarballs[index + 1]!;
    expect(entries(tarball)).toEqual([
      "package/LICENSE.md",
      "package/package.json",
      "package/stanza",
    ]);

    expect(packed(tarball, "stanza")).toBe(`binary for ${platform}`);

    const mode = Bun.spawnSync(["tar", "-tvzf", tarball, "package/stanza"]).stdout.toString();
    expect(mode).toStartWith("-rwxr-xr-x");
  }
});

test("pack refuses a platform package with no staged binary", () => {
  const base = scratch("packages-unstaged");
  cpSync(join(root, "package.json"), join(base, "package.json"));

  for (const platform of platforms)
    mkdirSync(packageDirectory(platform, base), { recursive: true });

  expect(() => pack(join(base, "out"), base)).toThrow(
    "npm/darwin-arm64 holds no binary: run stage first",
  );
});
