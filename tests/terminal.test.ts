import { expect, test } from "bun:test";
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { agentVariables, scratch, scratchGitRepository, stanzaBinary } from "./support.ts";

const orders = `export function total(orders: Order[]) {
  const sum = orders.reduce(
    (acc, order) => acc + order.amount,
    0,
  );
  return sum;
}

// the order with this id, or null
export function find(orders: Order[], id: string) {
  const index = orders.findIndex((order) => order.id === id);

  if (index < 0) return null;

  const order = orders[index];
  log(order);
  return order;
}
`;

const report = `export function report(rows: Row[]) {
  const header = title(rows);
  const total = sum(rows);
  const average = total / rows.length;
  const lines = [header];
  lines.push(\`total \${total}\`);
  lines.push(\`average \${average}\`);
  lines.push("");
  print(lines);
  for (const row of rows) {
    print(row.name);
    print(row.value);
  }
}
`;

const bold = (text: string) => `\x1b[1m${text}\x1b[0m`;
const dim = (text: string) => `\x1b[2m${text}\x1b[0m`;
const yellow = (text: string) => `\x1b[33m${text}\x1b[0m`;

const boldRed = (text: string) => `\x1b[1;31m${text}\x1b[0m`;
const boldYellow = (text: string) => `\x1b[1;33m${text}\x1b[0m`;
const sgr = new RegExp(`${"\x1b"}\\[[0-9;]*m`, "g");
const plain = (text: string) => text.replaceAll(sgr, "");

const terminalForm = [
  `${bold("src/orders.ts:6:3")}   ${dim("after-multiline")}  add a blank line after the multi-line statement above`,
  `${bold("src/orders.ts:13:3")}  ${dim("guard-join")}       remove the blank line between \`index\` and the \`if\` that reads it`,
  "",
  `${bold("src/report.ts:2:3")}   ${dim("wall")}             split these 8 statements into steps with a blank line  ${yellow("(by hand)")}`,
  `${bold("src/report.ts:10:3")}  ${dim("block-spacing")}    add a blank line above this block, or join it to the step above  ${yellow("(by hand)")}`,
  "",
  `${bold("4")} findings in ${bold("2")} files: ${bold("2")} fixable with --fix, ${bold("2")} by hand`,
  "",
].join("\n");

const pipeRows = (paint: boolean) => {
  const [b, d, y] = paint ? [bold, dim, yellow] : [plain, plain, plain];
  return [
    `${b("src/orders.ts:6:3")}: ${d("after-multiline")}: add a blank line after the multi-line statement above`,
    `${b("src/orders.ts:13:3")}: ${d("guard-join")}: remove the blank line between \`index\` and the \`if\` that reads it`,
    `${b("src/report.ts:2:3")}: ${d("wall")}: split these 8 statements into steps with a blank line ${y("(by hand)")}`,
    `${b("src/report.ts:10:3")}: ${d("block-spacing")}: add a blank line above this block, or join it to the step above ${y("(by hand)")}`,
    "",
  ].join("\n");
};

const summary = "4 findings in 2 files: 2 fixable with --fix, 2 by hand\n";

type Env = Record<string, string>;

function project(): string {
  const cwd = scratch("terminal");
  mkdirSync(join(cwd, "src"));
  writeFileSync(join(cwd, "src", "orders.ts"), orders);
  writeFileSync(join(cwd, "src", "report.ts"), report);
  return cwd;
}

function environment(vars: Env = {}): Env {
  return { PATH: process.env.PATH ?? "", HOME: process.env.HOME ?? "", ...vars };
}

type Aside = "none" | "stdout" | "stderr";

async function inTerminal(
  cwd: string,
  args: string[],
  vars: Env = {},
  aside: Aside = "none",
): Promise<{ screen: string; aside: string }> {
  const file = join(scratch("terminal-aside"), "aside");
  const redirect = { none: "", stdout: '>"$f"', stderr: '2>"$f"' }[aside];
  const chunks: Uint8Array[] = [];
  const { promise: closed, resolve } = Promise.withResolvers<void>();

  writeFileSync(file, "");
  const child = Bun.spawn(
    ["sh", "-c", `f=$1; shift; exec "$@" ${redirect}`, "sh", file, stanzaBinary(), ...args],
    {
      cwd,
      env: environment(vars),
      terminal: {
        cols: 400,
        data: (_, data) => chunks.push(data),
        exit: () => resolve(),
      },
    },
  );

  await child.exited;
  await closed;
  child.terminal?.close();

  return {
    screen: Buffer.concat(chunks).toString().replaceAll("\r\n", "\n"),
    aside: readFileSync(file, "utf8"),
  };
}

function piped(cwd: string, args: string[], vars: Env, stdin = ""): string {
  const result = Bun.spawnSync([stanzaBinary(), ...args], {
    cwd,
    env: environment(vars),
    stdin: new TextEncoder().encode(stdin),
  });

  return result.stdout.toString();
}

test("a terminal shows the findings aligned per file and colored, then the summary", async () => {
  const cwd = project();
  expect((await inTerminal(cwd, ["--check", "src"])).screen).toBe(terminalForm);
});

test("NO_COLOR and TERM=dumb keep the terminal layout without color", async () => {
  const cwd = project();
  for (const vars of [{ NO_COLOR: "1" }, { TERM: "dumb" }] as Env[])
    expect((await inTerminal(cwd, ["--check", "src"], vars)).screen).toBe(plain(terminalForm));
});

test("an agent variable gives pipe rows without color, whatever FORCE_COLOR says", async () => {
  const cwd = project();
  for (const name of agentVariables)
    for (const force of [{}, { FORCE_COLOR: "1" }] as Env[]) {
      const { screen } = await inTerminal(cwd, ["--check", "src"], { [name]: "1", ...force });
      expect(screen).toBe(`${pipeRows(false)}${summary}`);
    }
});

test("FORCE_COLOR colors piped rows unless it is 0 or empty", () => {
  const cwd = project();
  expect(piped(cwd, ["--check", "src"], { FORCE_COLOR: "1" })).toBe(pipeRows(true));
  expect(piped(cwd, ["--check", "src"], { FORCE_COLOR: "0" })).toBe(pipeRows(false));
  expect(piped(cwd, ["--check", "src"], { FORCE_COLOR: "" })).toBe(pipeRows(false));
});

test("FORCE_COLOR never colors JSON, stdin, explain, help or hook output", () => {
  const cwd = project();
  const repository = scratchGitRepository({ files: { "src/report.ts": report }, staged: true });
  const runs: [string, string[], string][] = [
    [cwd, ["--check", "--json", "src"], ""],
    [cwd, ["--check", "--stdin", "src/report.ts"], report],
    [cwd, ["--fix", "--stdin", "src/report.ts"], report],
    [cwd, ["explain", "src/report.ts:2"], ""],
    [cwd, ["--help"], ""],
    [repository, ["hook"], JSON.stringify({ cwd: repository })],
  ];

  for (const [dir, args, stdin] of runs) {
    const stdout = piped(dir, args, { FORCE_COLOR: "1" }, stdin);
    expect(stdout).not.toBe("");
    expect(stdout).not.toContain("\x1b");
  }
});

test.skipIf(process.getuid?.() === 0)(
  "a terminal stderr colors only the prefix, red for an error and yellow for a notice",
  async () => {
    const cwd = scratch("terminal-stderr");
    mkdirSync(join(cwd, "locked"), { mode: 0 });
    writeFileSync(join(cwd, "eslint.config.js"), "export default makeConfig();\n");
    writeFileSync(join(cwd, "a.ts"), "export const a = 1;\n");

    try {
      const failed = await inTerminal(cwd, ["--check", "locked"], {}, "stdout");
      expect(failed.screen).toBe(`${boldRed("stanza:")} cannot read locked: permission denied\n`);

      const noticed = await inTerminal(cwd, ["--check", "a.ts"], {}, "stdout");
      expect(noticed.screen).toBe(
        `${boldYellow("stanza:")} cannot tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it\nchecked ${bold("1")} file, no findings\n`,
      );
    } finally {
      chmodSync(join(cwd, "locked"), 0o755);
    }
  },
);

test("piped stdout keeps a terminal stderr colored and the rows plain", async () => {
  const cwd = project();
  const { screen, aside } = await inTerminal(cwd, ["--check", "src"], {}, "stdout");
  expect(aside).toBe(pipeRows(false));
  expect(screen).toBe(
    `${bold("4")} findings in ${bold("2")} files: ${bold("2")} fixable with --fix, ${bold("2")} by hand\n`,
  );
});

test("FORCE_COLOR tones the hook's prefix, red for a failure and yellow for a notice", () => {
  const repository = scratchGitRepository({
    files: {
      "eslint.config.js": "export default makeConfig();\n",
      "a.ts": "export const a = 1;\n",
    },
    staged: true,
  });

  const hook = (stdin: string) => {
    const result = Bun.spawnSync([stanzaBinary(), "hook"], {
      cwd: repository,
      env: environment({ FORCE_COLOR: "1" }),
      stdin: new TextEncoder().encode(stdin),
    });

    return { code: result.exitCode, stderr: result.stderr.toString() };
  };

  expect(hook("not json")).toEqual({
    code: 1,
    stderr: `${boldRed("stanza hook:")} input must be valid JSON\n`,
  });

  expect(hook(JSON.stringify({ cwd: repository }))).toEqual({
    code: 0,
    stderr: `${boldYellow("stanza hook:")} cannot tell whether eslint.config.js enforces braces, so braces stay; pass --braces or --no-braces to settle it\n`,
  });
});
