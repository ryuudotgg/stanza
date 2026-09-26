import { expect, test } from "bun:test";
import { blockReason, hookInput } from "../src/hook.ts";
import { RULES } from "../src/rules.ts";
import type { Finding } from "../src/types.ts";

function finding(path = "wall.ts", rule: "wall" | "block-spacing" | "parse" = "wall"): Finding {
  return {
    path,
    line: 2,
    col: 3,
    rule,
    message: rule === "parse" ? "Unexpected token" : RULES[rule].message(),
    fixable: false,
  };
}

test("hookInput defaults absent and null fields and ignores other fields", () => {
  for (const text of ["{}", '{"cwd":null,"stop_hook_active":null}', '{"event":"stop"}'])
    expect(hookInput(text, "/repo")).toEqual({ cwd: "/repo", stopHookActive: false });
});

test("hookInput resolves a relative cwd and accepts boolean stop_hook_active", () => {
  expect(hookInput('{"cwd":"../other","stop_hook_active":true}', "/repo/sub")).toEqual({
    cwd: "/repo/other",
    stopHookActive: true,
  });

  expect(hookInput('{"cwd":"/other","stop_hook_active":false}', "/repo")).toEqual({
    cwd: "/other",
    stopHookActive: false,
  });
});

test("hookInput rejects wrong input types and invalid JSON", () => {
  for (const text of ["null", "[]", '"input"', "true", "1"])
    expect(hookInput(text, "/repo")).toEqual({ error: "input must be a JSON object" });

  expect(hookInput('{"cwd":42}', "/repo")).toEqual({ error: "cwd must be a string" });
  expect(hookInput('{"stop_hook_active":"true"}', "/repo")).toEqual({
    error: "stop_hook_active must be a boolean",
  });

  expect(hookInput("not json", "/repo")).toEqual({ error: "input must be valid JSON" });
});

test("blockReason is absent without findings even when files were rewritten", () => {
  expect(blockReason([], ["bodies.ts"])).toBeUndefined();
});

test("blockReason prints the exact finding, summary and instruction", () => {
  expect(blockReason([finding()], [])).toBe(
    "stanza could not fix these in the files you changed:\n" +
      "  wall.ts:2:3 wall 6 or more statements with no blank line between them; separate the steps\n\n" +
      "wall: six or more consecutive single-line statements with no blank line\n\n" +
      "Fix those findings, then reply again.",
  );
});

test("blockReason caps finding lines at twelve and reports the remaining count", () => {
  const findings = Array.from({ length: 14 }, (_, index) => finding(`file${index}.ts`));
  const reason = blockReason(findings, ["file12.ts"]);

  expect(reason?.split("\n").filter((line) => line.startsWith("  file"))).toHaveLength(12);
  expect(reason).toContain("  file11.ts:2:3 wall ");
  expect(reason).not.toContain("  file12.ts");
  expect(reason).toEndWith("stanza rewrote file12.ts, so read it again before editing.");
  expect(reason).toContain("\n  ... and 2 more\n\n");

  expect(blockReason(findings.slice(0, 12), [])).not.toContain("... and");
  expect(blockReason(findings, ["other.ts"])).not.toContain("stanza rewrote");
});

test("blockReason gives parse findings their own message without a rule summary", () => {
  const parse = { ...finding("broken.ts", "parse"), line: 1, col: 10 };
  expect(blockReason([parse], [])).toBe(
    "stanza could not fix these in the files you changed:\n" +
      "  broken.ts:1:10 could not read or parse this file: Unexpected token\n\n" +
      "Fix those findings, then reply again.",
  );
});

test("blockReason tells an engine failure apart from a parse failure", () => {
  const failed = { ...finding("deep.ts", "parse"), rule: "error" as const, message: "RangeError" };
  expect(blockReason([failed], [])).toContain("deep.ts:2:3 stanza failed on this file: RangeError");
});

test("blockReason lists each rule once in catalog order", () => {
  const findings = [finding(), finding("steps.ts", "block-spacing"), finding("other.ts")];
  const reason = blockReason(findings, []);
  expect(reason?.split("\n\n")[1]).toBe(
    `block-spacing: ${RULES["block-spacing"].summary}\nwall: ${RULES.wall.summary}`,
  );
});

test("blockReason names only rewritten files with shown findings", () => {
  expect(blockReason([finding()], ["bodies.ts"])).not.toContain("stanza rewrote");
  expect(blockReason([finding()], ["bodies.ts", "wall.ts"])).toEndWith(
    "\n\nstanza rewrote wall.ts, so read it again before editing.",
  );

  const findings = [finding("a.ts"), finding("b.ts"), finding("c.ts")];
  expect(blockReason(findings, ["a.ts", "b.ts"])).toEndWith(
    "\n\nstanza rewrote a.ts and b.ts, so read them again before editing.",
  );

  expect(blockReason(findings, ["a.ts", "b.ts", "c.ts", "clean.ts"])).toEndWith(
    "\n\nstanza rewrote a.ts, b.ts and c.ts, so read them again before editing.",
  );
});
