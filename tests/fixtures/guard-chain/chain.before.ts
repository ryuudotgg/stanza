function referenceChild(node: Node, key: string): boolean {
  if (["id", "label"].includes(key)) return false;

  if (node.type.startsWith("TS")) return key === "expression";

  if (key === "key") return "computed" in node && node.computed === true;

  const fallback = lookup(node);

  if (fallback)
    return fallback;

  if (node.type === "Other") {
    log(node);
    return false;
  }

  return true;
}
