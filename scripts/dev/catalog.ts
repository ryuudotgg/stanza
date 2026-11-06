import { RULES } from "../../src/engine/rules.ts";
import { claudeCodeHooks } from "../../src/hook.ts";
import { extensions } from "../../src/languages/index.ts";
import { flags, usage } from "../../src/usage.ts";

function template(message: (...args: string[]) => string): string {
  const parameters = /^\(([^)]*)\)/.exec(message.toString())![1]!;
  const names = parameters
    .split(",")
    .map((name) => name.trim())
    .filter((name) => name !== "");

  return message(...names.map((name) => `{${name}}`));
}

export function catalog(): string {
  const rules = Object.fromEntries(
    Object.entries(RULES).map(([id, rule]) => [
      id,
      {
        fixable: rule.fixable,
        gap: rule.gap,
        summary: rule.summary,
        message: template(rule.message as (...args: string[]) => string),
      },
    ]),
  );

  return `${JSON.stringify({ rules, flags, usage, claudeCodeHooks, extensions }, null, 2)}\n`;
}

if (import.meta.main) process.stdout.write(catalog());
