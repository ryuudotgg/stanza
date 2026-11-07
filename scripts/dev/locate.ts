import { locate } from "../../src/files.ts";

if (import.meta.main)
  for (const dir of process.argv.slice(2)) console.log(JSON.stringify(locate(dir)));
