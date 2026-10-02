export interface PageText {
  path: string;
  raw: string;
  processed: string;
}

export interface Docs<Page> {
  pages: string[];
  sidebar: string[];
  texts: Record<string, Page>;
}

const docsDirectory = new URL("../docs/", import.meta.url).pathname;
function load<Page>(args: string[]): Docs<Page> {
  const result = Bun.spawnSync(
    ["bun", "--preload", "./scripts/preload.ts", "scripts/pages.ts", ...args],
    { cwd: docsDirectory },
  );

  if (result.exitCode !== 0)
    throw new Error(`Could not load docs pages: ${result.stderr.toString()}`);

  return JSON.parse(result.stdout.toString()) as Docs<Page>;
}

export const loadDocs = () => load<PageText>([]);

export const loadRenderedDocs = () => load<PageText & { html: string }>(["--html"]);
