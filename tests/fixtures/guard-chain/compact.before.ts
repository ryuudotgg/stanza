function pick(node: Node, key: string) {
  if (["id", "label"].includes(key))
    return false;

  if (node.type.startsWith("TS"))
    return key === "expression";

  if (node.type === "Other") {
    log(node);
    return false;
  }

  return true;
}
