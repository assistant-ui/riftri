import { defineConfig } from "@farm.js/core";
import { withDocs } from "@farming-labs/farmjs/config";

export default withDocs(defineConfig({
  vite: {
    plugins: [{
      name: "riftri-public-docs-only",
      enforce: "pre",
      transform(code: string, id: string) {
        if (!id.split("?")[0].replace(/\\/g, "/").endsWith("/@farming-labs/farmjs/dist/react.js")) return null;
        // Adapter 0.2.136 still globs every Markdown file in the project, one
        // lazy chunk per file, and has no option to scope it. Only staged public
        // guides belong in the UI; the full-reference `.md` companions under
        // public/docs stay raw assets, not adapter pages.
        const broadGlob = 'import.meta.glob("/**/*.{md,mdx}")';
        if (!code.includes(broadGlob)) throw new Error("Review the docs adapter Markdown glob before upgrading it.");
        return { code: code.replace(broadGlob, 'import.meta.glob("/src/app/docs/**/*.{md,mdx}")'), map: null };
      },
    }],
    // Dev pre-bundling would serve the adapter from .vite/deps, where the
    // transform above never sees its real path, so development would import
    // every Markdown file in the project.
    optimizeDeps: { exclude: ["@farming-labs/farmjs"] },
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
}), {
  // Wraps every docs page; it also hosts the "Copy .md" clipboard listener.
  shell: "src/components/docs-shell.tsx",
});
