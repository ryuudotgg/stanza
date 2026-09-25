import addon from "../node_modules/@oxc-parser/binding-darwin-arm64/parser.darwin-arm64.node" with { type: "file" };

// Must precede any oxc-parser import (its loader cannot find the binding in a compiled binary), hence the dynamic import.
process.env.NAPI_RS_NATIVE_LIBRARY_PATH = addon;
await import("./cli.ts");
