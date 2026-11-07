import { collectChanged } from "../../src/files.ts";

if (import.meta.main) {
  const args = process.argv.slice(2);
  const hunks = args[0] === "--hunks";
  const rest = args.slice(hunks ? 1 : 0);
  if (rest.length > 0 && rest[0] !== "--") {
    console.error(`unexpected argument ${rest[0]}`);
    process.exit(2);
  }

  const written = rest.length > 0 ? new Set(rest.slice(1)) : undefined;
  const { changedLines, ...selected } = collectChanged(process.cwd(), undefined, hunks, written);
  const lines = Object.fromEntries(
    [...changedLines].map(([path, changed]) => [
      path,
      { lines: [...changed.lines], deletedAfter: [...changed.deletedAfter] },
    ]),
  );

  console.log(JSON.stringify({ ...selected, changedLines: lines }));
}
