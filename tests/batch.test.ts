import { expect, test } from "bun:test";
import { chmodSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { batchClient } from "../scripts/batch.ts";
import { scratch } from "./support.ts";

test("a request holding an unpaired surrogate fails alone without reaching the binary", async () => {
  const directory = scratch("batch-surrogate");
  const binary = join(directory, "silent");
  writeFileSync(binary, "#!/bin/sh\ncat > /dev/null\n");
  chmodSync(binary, 0o755);

  const client = batchClient(binary);
  const answer = await client.ask({
    path: "a.ts",
    text: "const a = '\ud800';\n",
    mode: "check",
    braces: null,
    changedLines: null,
    explain: [],
  });

  expect(answer).toEqual({
    id: 0,
    text: null,
    findings: [],
    explanations: {},
    error: "a.ts: the request holds an unpaired surrogate, which JSON cannot carry to the binary",
  });

  expect(await client.close()).toEqual([]);
});
