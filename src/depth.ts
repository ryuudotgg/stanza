// Worker threads get a far smaller native stack than main and oxc's parser recurses, so a deep file aborts the whole process in a worker; these sit at an eighth of the crash depths measured in a worker.
export const FRAME_LIMIT = 300;
export const TOKEN_LIMIT = 1400;

const NEWLINE = 10;
const RETURN = 13;

const DOUBLE = 34;
const SINGLE = 39;
const BACKTICK = 96;
const DOLLAR = 36;
const BACKSLASH = 92;

const OPEN_PAREN = 40;
const OPEN_BRACKET = 91;
const OPEN_BRACE = 123;

const CLOSE_PAREN = 41;
const CLOSE_BRACKET = 93;
const CLOSE_BRACE = 125;

const SLASH = 47;
const STAR = 42;
const LESS = 60;
const GREATER = 62;

const COMMA = 44;
const SEMICOLON = 59;

const enum Frame {
  Bracket,
  Interpolation,
  Element,
}

const enum Last {
  Start,
  Name,
  Keyword,
  Literal,
  Close,
  Paren,
  Punctuator,
}

const REGEX_AFTER = [
  "return",
  "typeof",
  "instanceof",
  "in",
  "of",
  "new",
  "delete",
  "void",
  "throw",
  "case",
  "do",
  "else",
  "yield",
  "await",
];

const KEYWORDS = new Set([
  ...REGEX_AFTER,
  "keyof",
  "readonly",
  "infer",
  "unique",
  "async",
  "export",
  "declare",
  "abstract",
  "static",
  "public",
  "private",
  "protected",
  "override",
  "get",
  "set",
  "accessor",
  "let",
  "const",
  "var",
  "function",
  "class",
  "interface",
  "type",
  "enum",
  "namespace",
  "module",
  "import",
  "from",
  "default",
  "extends",
  "implements",
  "as",
  "satisfies",
  "is",
  "asserts",
  "if",
  "for",
  "while",
  "with",
  "switch",
  "try",
  "catch",
  "finally",
]);

const CONTINUES = new Set([
  "else",
  "catch",
  "finally",
  "while",
  "instanceof",
  "in",
  "of",
  "as",
  "satisfies",
  "extends",
  "implements",
]);

interface Scan {
  text: string;
  frames: Frame[];
  segments: number[];
  tokens: number;
}

function isWord(code: number): boolean {
  return (
    (code >= 97 && code <= 122) ||
    (code >= 65 && code <= 90) ||
    (code >= 48 && code <= 57) ||
    code === 95 ||
    code === DOLLAR ||
    code > 127
  );
}

function isBlank(code: number): boolean {
  return (
    code === 32 || code === 9 || code === 11 || code === 12 || code === 0xa0 || code === 0xfeff
  );
}

function isLineEnd(code: number): boolean {
  return code === NEWLINE || code === RETURN;
}

function quotedEnd(text: string, start: number, quote: number): number {
  for (let index = start + 1; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code === quote) return index + 1;
    if (code === BACKSLASH) index++;
    else if (isLineEnd(code)) return -1;
  }

  return -1;
}

function regexEnd(text: string, start: number): number {
  let inClass = false;
  for (let index = start + 1; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code === BACKSLASH) index++;
    else if (isLineEnd(code)) return -1;
    else if (code === OPEN_BRACKET) inClass = true;
    else if (code === CLOSE_BRACKET) inClass = false;
    else if (code === SLASH && !inClass) {
      let end = index + 1;
      while (end < text.length && isWord(text.charCodeAt(end))) end++;
      return end;
    }
  }

  return -1;
}

function count(scan: Scan): boolean {
  scan.segments[scan.segments.length - 1]!++;
  scan.tokens++;

  const nested = (scan.frames.length - 1) * TOKEN_LIMIT;
  return nested + scan.tokens * FRAME_LIMIT > FRAME_LIMIT * TOKEN_LIMIT;
}

function reset(scan: Scan): void {
  scan.tokens -= scan.segments[scan.segments.length - 1]!;
  scan.segments[scan.segments.length - 1] = 0;
}

function push(scan: Scan, frame: Frame): void {
  scan.frames.push(frame);
  scan.segments.push(0);
}

function pop(scan: Scan): void {
  scan.tokens -= scan.segments.pop()!;
  scan.frames.pop();
}

function top(scan: Scan): Frame {
  return scan.frames[scan.frames.length - 1]!;
}

function closeElement(scan: Scan): void {
  pop(scan);
  if (top(scan) === Frame.Element) reset(scan);
}

function templateEnd(scan: Scan, start: number): number {
  const { text } = scan;
  for (let index = start; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code === BACKSLASH) index++;
    else if (code === BACKTICK) return index + 1;
    else if (code === DOLLAR && text.charCodeAt(index + 1) === OPEN_BRACE) {
      push(scan, Frame.Interpolation);
      return -(index + 2);
    }
  }

  return text.length;
}

export function deepRisk(text: string, jsx: boolean): boolean {
  const scan: Scan = { text, frames: [Frame.Bracket], segments: [0], tokens: 0 };

  let last = Last.Start;
  let lineBreak = false;
  let index = 0;
  while (index < text.length) {
    const code = text.charCodeAt(index);
    if (isBlank(code)) {
      index++;
      continue;
    }

    if (isLineEnd(code)) {
      lineBreak ||= last === Last.Name || last === Last.Literal || last === Last.Close;
      index++;
      continue;
    }

    const next = text.charCodeAt(index + 1);
    if (code === SLASH && next === SLASH) {
      while (index < text.length && !isLineEnd(text.charCodeAt(index))) index++;
      continue;
    }

    const commentEnd = code === SLASH && next === STAR ? text.indexOf("*/", index + 2) : -1;
    if (commentEnd >= 0) {
      index = commentEnd + 2;
      continue;
    }

    if (isWord(code)) {
      const start = index;
      while (index < text.length && isWord(text.charCodeAt(index))) index++;

      const word = index - start <= 10 ? text.slice(start, index) : "";
      if (lineBreak && !CONTINUES.has(word)) reset(scan);

      lineBreak = false;
      last = KEYWORDS.has(word) ? Last.Keyword : Last.Name;
      if (count(scan)) return true;
      continue;
    }

    lineBreak = false;

    if (code === SINGLE || code === DOUBLE) {
      const end = quotedEnd(text, index, code);
      index = end < 0 ? index + 1 : end;
      last = end < 0 ? Last.Punctuator : Last.Literal;
      if (count(scan)) return true;
      continue;
    }

    if (code === BACKTICK) {
      const end = templateEnd(scan, index + 1);
      index = Math.abs(end);
      last = end < 0 ? Last.Punctuator : Last.Literal;
      if (count(scan)) return true;
      continue;
    }

    const divides =
      last === Last.Name || last === Last.Literal || last === Last.Close || last === Last.Paren;

    const regex = code === SLASH && !divides ? regexEnd(text, index) : -1;
    if (regex >= 0) {
      index = regex;
      last = Last.Literal;
      if (count(scan)) return true;
      continue;
    }

    index++;

    if (code === SEMICOLON || code === COMMA) {
      reset(scan);
      last = Last.Punctuator;
      continue;
    }

    if (code === OPEN_PAREN || code === OPEN_BRACKET || code === OPEN_BRACE) {
      if (count(scan)) return true;

      push(scan, Frame.Bracket);
      last = Last.Punctuator;
      continue;
    }

    if (code === CLOSE_PAREN || code === CLOSE_BRACKET || code === CLOSE_BRACE) {
      while (scan.frames.length > 1 && top(scan) === Frame.Element) pop(scan);

      if (code === CLOSE_BRACE && scan.frames.length > 1 && top(scan) === Frame.Interpolation) {
        pop(scan);

        const end = templateEnd(scan, index);
        index = Math.abs(end);
        last = end < 0 ? Last.Punctuator : Last.Literal;
        continue;
      }

      if (scan.frames.length > 1) pop(scan);

      last = code === CLOSE_PAREN ? Last.Paren : Last.Close;
      if (count(scan)) return true;
      continue;
    }

    if (jsx && code === LESS && next === SLASH) {
      if (top(scan) === Frame.Element) closeElement(scan);

      last = Last.Punctuator;
      if (count(scan)) return true;
      continue;
    }

    const opensElement =
      top(scan) === Frame.Element ||
      last === Last.Start ||
      last === Last.Punctuator ||
      last === Last.Keyword;

    if (jsx && code === LESS && opensElement && (isWord(next) || next === GREATER)) {
      if (count(scan)) return true;

      push(scan, Frame.Element);
      last = Last.Punctuator;
      continue;
    }

    if (jsx && code === SLASH && next === GREATER && top(scan) === Frame.Element) {
      index++;
      closeElement(scan);
    }

    last = Last.Punctuator;
    if (count(scan)) return true;
  }

  return false;
}
