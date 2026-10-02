import {
  readFileSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { a } from "./a.ts";
import { b } from "./b.ts";
import { c } from "./c.ts";
import { d } from "./d.ts";
export * from "./e.ts";

const limit = 10;
export function cap(value: number): number {
  return Math.min(value, limit);
}

export const total = cap(a + b + c + d);
export default function save(path: string): void {
  writeFileSync(join(path, "total"), String(total));
  readFileSync(path);
}
