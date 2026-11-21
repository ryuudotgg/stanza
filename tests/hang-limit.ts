import { setDefaultTimeout } from "bun:test";

// Above the 120 second child limit in support.ts, so a hung binary fails with the
// runner's message. A `timeout` key in bunfig.toml leaves Bun's 5 seconds in place.
setDefaultTimeout(180_000);
