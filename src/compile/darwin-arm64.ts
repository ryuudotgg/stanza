import addon from "../../node_modules/@oxc-parser/binding-darwin-arm64/parser.darwin-arm64.node" with { type: "file" };
import { start } from "../compiled.ts";
await start(addon);
