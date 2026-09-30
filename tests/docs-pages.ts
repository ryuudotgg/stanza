interface PageText {
  path: string;
  raw: string;
  processed: string;
  html: string;
}

const docsDirectory = new URL("../docs/", import.meta.url).pathname;
const result = Bun.spawnSync(["bun", "--preload", "./scripts/preload.ts", "scripts/pages.ts"], {
  cwd: docsDirectory,
});

if (result.exitCode !== 0)
  throw new Error(`Could not load docs pages: ${result.stderr.toString()}`);

export const { pages, sidebar, texts } = JSON.parse(result.stdout.toString()) as {
  pages: string[];
  sidebar: string[];
  texts: Record<string, PageText>;
};
