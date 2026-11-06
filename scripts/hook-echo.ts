import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const table = join(root, "tests", "fixtures", "hook", "updated-input.json");
const content = JSON.stringify("function f() {\n  const x = {\n    a,\n  };\n  log(x);\n}\n");

function write(fields: string): string {
  return `{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{${fields}}}`;
}

const target = `"file_path":"a.ts","content":${content}`;

function bytes(...parts: (string | number[])[]): Uint8Array {
  return Buffer.concat(parts.map((part) => Buffer.from(part)));
}

const INPUTS: [string, string | Uint8Array][] = [
  ["plain", write(target)],
  [
    "integer like keys",
    write(
      `"b":1,"2":"two",${target},"a":2,"1":true,"10":null,"01":0,"-1":0,"1.5":0,"4294967295":1,"4294967294":2,"0":[]`,
    ),
  ],
  ["content before file_path", write(`"content":${content},"z":1,"file_path":"a.ts","3":3`)],
  [
    "duplicate keys",
    write(`"x":1,"y":2,"x":3,"content":"draft",${target},"file_path":"a.ts","y":{"k":1,"k":2}`),
  ],
  [
    "floats",
    write(`${target},"f":[1.5,-0.0,-0,0.1,0.30000000000000004,123456789.125,1.0,100.00,2.50]`),
  ],
  [
    "exponents",
    write(
      `${target},"e":[1e21,1E+21,1e20,2.5e-7,1e-7,1e-6,123e-20,1e400,-1e400,1e-400,5e-324,1.7976931348623157e308,1e+0]`,
    ),
  ],
  [
    "integers past 2^53",
    write(
      `${target},"i":[9007199254740993,9007199254740992,12345678901234567890,-9007199254740993,18446744073709551616,123456789012345678901234567890]`,
    ),
  ],
  [
    "unicode escapes",
    write(
      `${target},"u":["\\u00e9","\\u00E9","\\ud83d\\ude00","\\ud800","\\udc00","a\\ud800b","\\u0000","\\u001f","\\u007f","\\u2028\\u2029","\\"\\\\\\/","\\b\\f\\n\\r\\t","é😀"]`,
    ),
  ],
  ["unicode keys", write(`${target},"\\u00e9":1,"é":2,"\\ud800":3,"":4,"__proto__":{"0":1}`)],
  [
    "nested key order",
    write(`${target},"n":{"b":{"2":1,"a":2,"1":3},"0":[{"z":1,"9":2}],"a":[[],{}]}`),
  ],
  ["literals", write(`${target},"t":true,"f":false,"n":null,"s":"","a":[],"o":{}`)],
  [
    "whitespace",
    ` \r\n\t{ "hook_event_name" : "PreToolUse" ,\n"tool_name":"Write","tool_input":{ ${target} , "w" : [ 1 , 2 ] } }\n\t `,
  ],
  [
    "lone surrogate in content",
    write(
      `"file_path":"a.ts","content":"const s = \\"\\ud800\\";\\n// \\udc00\\n${content.slice(1, -1)}"`,
    ),
  ],
  ["file_path that needs escaping", write(`"file_path":"a \\"q\\"\\t.ts","content":${content}`)],
  ["non-string content overridden", write(`"content":42,"file_path":"a.ts","content":${content}`)],
  [
    "invalid utf-8",
    bytes(write(`${target},"bytes":"a`).slice(0, -2), [0xff, 0xc3, 0x28, 0xed, 0xa0, 0x80], `"}}`),
  ],
];

const cli = join(root, "src", "cli.ts");
const rows = INPUTS.map(([name, input]) => {
  const cwd = realpathSync(mkdtempSync(join(tmpdir(), "stanza-hook-echo-")));
  try {
    const init = Bun.spawnSync(["git", "init", "-q"], { cwd });
    if (init.exitCode !== 0) throw new Error(`git init failed in ${cwd}`);

    const stdin = Buffer.from(input);
    const result = Bun.spawnSync(["bun", "--no-env-file", "--config=/dev/null", cli, "hook"], {
      cwd,
      stdin,
      env: { ...process.env, AGENT_HOOKS: "1" },
    });

    const stdout = result.stdout.toString();
    const stderr = result.stderr.toString();
    if (result.exitCode !== 0 || stderr !== "" || stdout === "")
      throw new Error(`${name}: exit ${result.exitCode}, stderr ${stderr}, stdout ${stdout}`);

    return { name, stdin: stdin.toString("hex"), stdout };
  } finally {
    rmSync(cwd, { recursive: true, force: true });
  }
});

writeFileSync(table, `${JSON.stringify(rows, null, 2)}\n`);
console.log(`hook-echo: recorded ${rows.length} inputs in ${table}`);
