type Rule = { summary: string; message: (...args: never[]) => string } & (
  | { fixable: true; gap: "none" | "at-least-one" | null }
  | { fixable: false; gap: null }
);

const joinMessage = (name: string, reader: string) =>
  `keep \`${name}\` next to the \`${reader}\` that reads it (remove the blank line)`;

export const RULES = {
  "after-multiline": {
    fixable: true,
    gap: "at-least-one",
    summary: "a statement that spans several lines is followed by a blank line",
    message: () => "blank line expected after the multi-line statement above",
  },
  "switch-clauses": {
    fixable: true,
    gap: "at-least-one",
    summary:
      "one blank line between switch clauses; a fall through label with an empty body stays directly above the next label; a blank line between clauses is never removed",
    message: () => "blank line expected between switch clauses",
  },
  "edge-blank": {
    fixable: true,
    gap: null,
    summary: "no blank line right after `{` or right before `}`",
    message: (side: "after {" | "before }") => `no blank line right ${side}`,
  },
  "guard-join": {
    fixable: true,
    gap: "none",
    summary:
      "a single-line declaration or assignment is followed directly by an `if` that references what it binds, whatever the body size and with or without `else`",
    message: joinMessage,
  },
  "consume-join": {
    fixable: true,
    gap: "none",
    summary:
      "a single-line declaration or assignment is followed directly by a `return` or `switch` that references what it binds",
    message: joinMessage,
  },
  "use-join": {
    fixable: true,
    gap: "none",
    summary:
      "a single-line declaration is followed directly by a loop, `try` or function declaration that references what it binds",
    message: joinMessage,
  },
  "guard-chain": {
    fixable: true,
    gap: "none",
    summary:
      "consecutive single-line guards (`if` with a one statement body and no `else`) have no blank line between them",
    message: () => "no blank line between consecutive single-line guards",
  },
  "let-step": {
    fixable: true,
    gap: "at-least-one",
    summary:
      "a `let` that joins the block below it gets a blank line above it, so it starts its own step",
    message: () => "blank line expected above a let that the block below assigns or uses",
  },
  "after-guard": {
    fixable: true,
    gap: "at-least-one",
    summary:
      "a single-line guard that returns, throws, continues or breaks is followed by a blank line, unless the next statement is an `if` or a jump (`return`, `throw`, `break`, `continue`)",
    message: () =>
      "blank line expected after the guard above, the next statement starts a new step",
  },
  "short-body": {
    fixable: true,
    gap: "none",
    summary:
      "a block of two or three single-line statements has no blank lines, whether it is a function body, a nested block or a switch clause body. A braceless `if` or loop whose header and body each sit on one line counts as single-line here, because the formatter puts it on one line",
    message: () => "no blank lines between 2 or 3 single-line statements",
  },
  braces: {
    fixable: true,
    gap: null,
    summary: "a control flow body that is a single statement has no braces",
    message: () => "braces around a single statement body",
  },
  "block-spacing": {
    fixable: false,
    gap: null,
    summary:
      "a multi-line block directly under a statement, when no join rule explains it. Guard chains, parallel `if` runs and the `setBusy(true)` then `try { } finally { setBusy(false) }` bracket are not reported",
    message: () => "blank line expected before this block, or join it to the step above",
  },
  wall: {
    fixable: false,
    gap: null,
    summary: "six or more consecutive single-line statements with no blank line",
    message: () => "6 or more statements with no blank line between them; separate the steps",
  },
} as const satisfies Record<string, Rule>;

export type RuleId = keyof typeof RULES;

export type GapRule<W extends Rule["gap"]> = {
  [K in RuleId]: (typeof RULES)[K]["gap"] extends W ? K : never;
}[RuleId];
