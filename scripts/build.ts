import { readdirSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { hostPlatform } from "./platform.ts";

const root = join(import.meta.dir, "..");
const compileDirectory = join(root, "src", "compile");
const platforms = readdirSync(compileDirectory)
  .filter((file) => file.endsWith(".ts"))
  .map((file) => file.slice(0, -3))
  .sort();

async function build(platform: string, outfile: string): Promise<void> {
  const entry = join(compileDirectory, `${platform}.ts`);
  const target = `bun-${platform}` as Bun.Build.CompileTarget;
  const result = await Bun.build({
    entrypoints: [entry],
    compile: { target, outfile },
    minify: true,
    target: "bun",
  });

  if (!result.success) throw new Error(result.logs.map((log) => log.message).join("\n"));
  if (!platform.startsWith("darwin")) return;

  if (process.platform === "darwin") await Bun.$`codesign -s - -f ${outfile}`;
  else
    console.warn(
      `${outfile} is unsigned: run codesign -s - -f on it on a Mac before it will start`,
    );
}

const { values } = parseArgs({
  args: process.argv.slice(2),
  options: { platform: { type: "string", multiple: true } },
  strict: true,
});

const requestedPlatforms = values.platform ?? [];
const unknown = requestedPlatforms.filter(
  (platform) => platform !== "all" && !platforms.includes(platform),
);

if (unknown.length > 0) {
  console.error(
    `Unknown platform ${unknown.join(", ")}. Valid platforms: ${[...platforms, "all"].join(", ")}`,
  );

  process.exit(1);
}

if (requestedPlatforms.length === 0) {
  const platform = hostPlatform();
  if (!platforms.includes(platform)) {
    console.error(`Unsupported platform ${platform}. Supported platforms: ${platforms.join(", ")}`);
    process.exit(1);
  }

  await build(platform, join(root, "bin", "stanza"));
} else {
  const selectedPlatforms = new Set(
    requestedPlatforms.flatMap((platform) => (platform === "all" ? platforms : [platform])),
  );

  await Bun.$`bun install --os='*' --cpu='*'`.cwd(root);

  for (const platform of platforms)
    if (selectedPlatforms.has(platform))
      await build(platform, join(root, "bin", `stanza-${platform}`));
}
