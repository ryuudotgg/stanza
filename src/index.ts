import { braceEdits } from "./braces.ts";
import { document, finding } from "./doc.ts";
import { applyLines, applyOffsets } from "./edits.ts";
import { spacing } from "./gaps.ts";
import { parse } from "./parse.ts";
import type { FileResult, Finding, Mode, Options } from "./types.ts";

function sorted(findings: Finding[]): Finding[] {
  return findings.sort(
    (left, right) =>
      left.line - right.line || left.col - right.col || left.rule.localeCompare(right.rule),
  );
}

export function processFile(path: string, text: string, mode: Mode, options: Options): FileResult {
  const parsed = parse(path, text);
  const doc = document(path, text, parsed);
  const error = parsed.errors[0];
  if (error)
    return {
      text,
      findings: [finding(doc, error.labels[0]?.start ?? 0, "parse", error.message, false)],
      parseError: true,
    };

  const braces = options.enforcedBraces ? { edits: [], findings: [] } : braceEdits(doc);
  if (mode === "check")
    return {
      text,
      findings: sorted([...braces.findings, ...spacing(doc).findings]),
      parseError: false,
    };

  let unbraced = text;

  let unbracedDoc = doc;
  for (
    let edits = braces.edits;
    edits.length > 0 && !options.enforcedBraces;
    edits = braceEdits(unbracedDoc).edits
  ) {
    unbraced = applyOffsets(unbraced, edits);
    unbracedDoc = document(path, unbraced, parse(path, unbraced));
  }

  const spaced = applyLines(unbracedDoc, spacing(unbracedDoc).edits);
  const finalDoc = spaced === unbraced ? unbracedDoc : document(path, spaced, parse(path, spaced));

  const findings = spacing(finalDoc).findings.filter((item) => !item.fixable);
  return { text: spaced, findings: sorted(findings), parseError: false };
}
