import { collectFiles } from "../../src/files.ts";

if (import.meta.main) {
  const args = process.argv.slice(2);
  const keepGenerated = args[0] === "--keep-generated";
  const paths = args.slice(keepGenerated ? 1 : 0);
  const selected = collectFiles(
    paths[0] === "--" ? paths.slice(1) : paths,
    process.cwd(),
    keepGenerated,
  );

  console.log(JSON.stringify(selected));
}
