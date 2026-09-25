import addon from "../../node_modules/@oxc-parser/binding-linux-x64-gnu/parser.linux-x64-gnu.node" with { type: "file" };
import { start } from "../compiled.ts";
await start(addon);
