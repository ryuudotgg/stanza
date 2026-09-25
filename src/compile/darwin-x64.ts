import addon from "../../node_modules/@oxc-parser/binding-darwin-x64/parser.darwin-x64.node" with { type: "file" };
import { start } from "../compiled.ts";
await start(addon);
