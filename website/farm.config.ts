import { defineConfig } from "@farm.js/core";
import { withDocs } from "@farming-labs/farmjs/config";

export default withDocs(defineConfig({
  vite: {
    plugins: [{
      name: "riftri-public-docs-only",
      enforce: "pre",
      transform(code: string, id: string) {
        if (!id.split("?")[0].replace(/\\/g, "/").endsWith("/@farming-labs/farmjs/dist/react.js")) return null;
        // Adapter 0.2.115 eagerly imports every Markdown file in the project.
        // Only staged public guides belong in the UI; agent.md stays a raw asset.
        const broadGlob = 'import.meta.glob("/**/*.{md,mdx}",';
        if (!code.includes(broadGlob)) throw new Error("Review the docs adapter Markdown glob before upgrading it.");
        return { code: code.replace(broadGlob, 'import.meta.glob("/src/app/docs/**/*.{md,mdx}",'), map: null };
      },
    }],
  },
  theme: {
    default: "dark",
  },
  images: {
    provider: "none",
  },
  deploy: {
    target: "vercel",
  },
}));
