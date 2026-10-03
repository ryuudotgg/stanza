import { llms, loader } from "fumadocs-core/source";
import { lucideIconsPlugin } from "fumadocs-core/source/lucide-icons";
import { docsRoute } from "./shared";
import { defineDocs } from "fumadocs-mdx/macro";
import { applyMdxPreset } from "fumadocs-mdx/config";
import { rehypeCodeDefaultOptions } from "fumadocs-core/mdx-plugins";
import { metaSchema, pageSchema } from "fumadocs-core/source/schema";
import { remarkDiffExample, transformerDiffExample } from "./diff";
import { remarkRules } from "./rules";
import { remarkUsage } from "./usage";

const docs = defineDocs({
  dir: "content/docs",
  meta: { schema: metaSchema },
  docs: {
    schema: pageSchema,
    mdxOptions: applyMdxPreset({
      remarkPlugins: [remarkRules, remarkUsage, remarkDiffExample],
      rehypeCodeOptions: {
        ...rehypeCodeDefaultOptions,
        engine: "oniguruma",
        transformers: [...(rehypeCodeDefaultOptions.transformers ?? []), transformerDiffExample()],
      },
    }),
    postprocess: { includeProcessedMarkdown: true },
  },
});

export const source = loader({
  baseUrl: docsRoute,
  source: docs.toFumadocsSource(),
  plugins: [lucideIconsPlugin()],
});

export const docsLlms = llms(source, {
  renderPage: async (page) => `# ${page.data.title} (${page.url})

${await page.data.getText("processed")}`,
});
