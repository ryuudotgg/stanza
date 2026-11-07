import { stdinTarget } from "../../src/files.ts";

if (import.meta.main)
  for (const path of process.argv.slice(2))
    console.log(JSON.stringify(stdinTarget(path, process.cwd())));
