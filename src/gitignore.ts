type CharacterSet = {
  singles: string[];
  ranges: [string, string][];
  classes: string[];
  negated: boolean;
};

type Token =
  | { kind: "literal"; value: string }
  | { kind: "star" | "globstar" | "directories" | "any" }
  | { kind: "set"; value: CharacterSet };

export interface Rule {
  tokens: Token[];
  anchored: boolean;
  directory: boolean;
  negated: boolean;
}

const posixClasses = new Set([
  "alnum",
  "alpha",
  "blank",
  "cntrl",
  "digit",
  "graph",
  "lower",
  "print",
  "punct",
  "space",
  "upper",
  "xdigit",
]);

function characterSet(pattern: string, start: number): { token: Token; end: number } | undefined {
  let index = start + 1;
  const negated = pattern[index] === "!" || pattern[index] === "^";
  if (negated) index++;

  const singles: string[] = [];
  const ranges: [string, string][] = [];
  const classes: string[] = [];

  let first = true;
  let previous: string | undefined;
  while (index < pattern.length) {
    if (pattern[index] === "]" && !first) {
      if (previous !== undefined) singles.push(previous);
      return {
        token: { kind: "set", value: { singles, ranges, classes, negated } },
        end: index + 1,
      };
    }

    first = false;
    const classMatch = pattern.slice(index).match(/^\[:([a-z]+):\]/);
    if (classMatch && posixClasses.has(classMatch[1]!)) {
      if (previous !== undefined) singles.push(previous);
      previous = undefined;
      classes.push(classMatch[1]!);
      index += classMatch[0].length;
      continue;
    }

    let character = pattern[index]!;
    const escaped = character === "\\";
    if (escaped) {
      index++;
      if (index === pattern.length) return undefined;
      character = pattern[index]!;
    }

    if (
      !escaped &&
      character === "-" &&
      previous !== undefined &&
      pattern[index + 1] !== "]" &&
      index + 1 < pattern.length
    ) {
      index++;

      let end = pattern[index]!;
      if (end === "\\") {
        index++;
        if (index === pattern.length) return undefined;
        end = pattern[index]!;
      }

      ranges.push([previous, end]);
      previous = undefined;
    } else {
      if (previous !== undefined) singles.push(previous);
      previous = character;
    }

    index++;
  }

  return undefined;
}

function tokenize(pattern: string): Token[] | undefined {
  const tokens: Token[] = [];
  for (let index = 0; index < pattern.length;) {
    const character = pattern[index]!;
    if (character === "\\") {
      if (index + 1 === pattern.length) return undefined;
      tokens.push({ kind: "literal", value: pattern[index + 1]! });
      index += 2;
    } else if (character === "[") {
      const parsed = characterSet(pattern, index);
      if (!parsed) return undefined;

      tokens.push(parsed.token);
      index = parsed.end;
    } else if (character === "*") {
      const start = index;
      while (pattern[index] === "*") index++;

      const run = index - start;
      const afterBoundary = start === 0 || pattern[start - 1] === "/";
      if (run > 1 && afterBoundary && pattern[index] === "/") {
        tokens.push({ kind: "directories" });
        index++;
      } else if (run > 1 && afterBoundary && index === pattern.length)
        tokens.push({ kind: "globstar" });
      else tokens.push({ kind: "star" });
    } else {
      tokens.push(character === "?" ? { kind: "any" } : { kind: "literal", value: character });
      index++;
    }
  }

  return tokens;
}

function bytes(text: string): string {
  return Buffer.from(text, "utf8").toString("latin1");
}

export function parseIgnore(text: string): Rule[] {
  const rules: Rule[] = [];
  for (let line of bytes(text).split("\n")) {
    if (line.endsWith("\r")) line = line.slice(0, -1);
    if (!line || line.startsWith("#")) continue;

    let trailing = line.length;
    while (trailing > 0 && line[trailing - 1] === " ") {
      let slashes = 0;
      for (let index = trailing - 2; index >= 0 && line[index] === "\\"; index--) slashes++;

      if (slashes % 2 === 1) break;

      trailing--;
    }

    line = line.slice(0, trailing);
    if (!line) continue;

    const negated = line.startsWith("!");
    if (negated) line = line.slice(1);

    const directory = line.endsWith("/");
    if (directory) line = line.slice(0, -1);

    const anchored = line.includes("/");
    if (line.startsWith("/")) line = line.slice(1);

    const tokens = tokenize(line);
    if (tokens) rules.push({ tokens, anchored, directory, negated });
  }

  return rules;
}

function inClass(name: string, character: string): boolean {
  const code = character.charCodeAt(0);
  const upper = code >= 65 && code <= 90;
  const lower = code >= 97 && code <= 122;
  const digit = code >= 48 && code <= 57;
  switch (name) {
    case "alnum":
      return upper || lower || digit;

    case "alpha":
      return upper || lower;

    case "blank":
      return character === " " || character === "\t";

    case "cntrl":
      return code < 32 || code === 127;

    case "digit":
      return digit;

    case "graph":
      return code >= 33 && code <= 126;

    case "lower":
      return lower;

    case "print":
      return code >= 32 && code <= 126;

    case "punct":
      return code >= 33 && code <= 126 && !upper && !lower && !digit;

    case "space":
      return character === " " || (code >= 9 && code <= 13);

    case "upper":
      return upper;

    case "xdigit":
      return digit || (code >= 65 && code <= 70) || (code >= 97 && code <= 102);
  }

  return false;
}

function matches(tokens: Token[], path: string): boolean {
  const length = path.length;

  let next = Array.from({ length: length + 1 }, (_, index) => index === length);
  for (let tokenIndex = tokens.length - 1; tokenIndex >= 0; tokenIndex--) {
    const token = tokens[tokenIndex]!;
    const current = Array<boolean>(length + 1).fill(false);
    const throughSlash = Array<boolean>(length + 1).fill(false);
    for (let index = length; index >= 0; index--) {
      const character = path[index];
      if (token.kind === "literal") current[index] = character === token.value && next[index + 1]!;
      else if (token.kind === "any")
        current[index] = character !== undefined && character !== "/" && next[index + 1]!;
      else if (token.kind === "set") {
        if (character !== undefined && character !== "/") {
          const set = token.value;
          const included =
            set.singles.includes(character) ||
            set.ranges.some(([start, end]) => start <= character && character <= end) ||
            set.classes.some((name) => inClass(name, character));

          current[index] = (set.negated ? !included : included) && next[index + 1]!;
        }
      } else if (token.kind === "directories") {
        throughSlash[index] =
          character !== undefined &&
          ((character === "/" && next[index + 1]!) || throughSlash[index + 1]!);

        current[index] = next[index]! || throughSlash[index]!;
      } else
        current[index] =
          next[index]! ||
          (character !== undefined &&
            (token.kind === "globstar" || character !== "/") &&
            current[index + 1]!);
    }

    next = current;
  }

  return next[0]!;
}

export function ignoredByRules(
  rules: Rule[],
  path: string,
  isDirectory: boolean,
): boolean | undefined {
  const target = bytes(path);

  let ignored: boolean | undefined;
  for (const rule of rules) {
    if (rule.directory && !isDirectory) continue;
    if (matches(rule.tokens, rule.anchored ? target : target.slice(target.lastIndexOf("/") + 1)))
      ignored = !rule.negated;
  }

  return ignored;
}
