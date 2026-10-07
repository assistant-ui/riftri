import { defineConfig } from "@farm.js/core";
import { withDocs } from "@farming-labs/farmjs/config";
import type { IncomingMessage, ServerResponse } from "node:http";

// The two dev-server members the Markdown middleware below uses, typed
// structurally because Farm's types and this site resolve different Vite majors.
type DevServer = {
  middlewares: {
    use(handler: (req: IncomingMessage, res: ServerResponse, next: (error?: unknown) => void) => void): unknown;
  };
  transformRequest(url: string): Promise<{ code: string } | null>;
};

// Client-only snippet appended to the docs adapter (react.js). It runs once in
// the browser and delegates clicks on the "Copy .md" link to a clipboard copy
// of the page's own `.md`, falling back to opening it when copying is
// unavailable. Kept as a plain string so it is bundled verbatim into the client.
const COPY_MARKDOWN_CLIENT = `
;(function () {
  if (typeof document === "undefined" || window.__riftriCopyMarkdown) return;
  window.__riftriCopyMarkdown = true;
  var SELECTOR = 'a[title="Copy this page as Markdown"]';
  document.addEventListener("click", function (event) {
    var link = event.target && event.target.closest ? event.target.closest(SELECTOR) : null;
    if (!link || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    if (!navigator.clipboard || !navigator.clipboard.writeText) return;
    event.preventDefault();
    event.stopPropagation();
    var href = link.getAttribute("href");
    // Snapshot the original label exactly once so repeated clicks within the
    // restore window can never capture the transient "Copied .md" text and
    // leave the link stuck on it.
    var label = link.dataset.riftriCopyLabel;
    if (label == null) { label = link.dataset.riftriCopyLabel = link.textContent; }
    fetch(href, { headers: { Accept: "text/plain" } })
      .then(function (response) { if (!response.ok) throw new Error(String(response.status)); return response.text(); })
      .then(function (markdown) { return navigator.clipboard.writeText(markdown); })
      .then(function () {
        link.textContent = "Copied .md";
        window.clearTimeout(link.__riftriCopyTimer);
        link.__riftriCopyTimer = window.setTimeout(function () { link.textContent = label; }, 1800);
      })
      .catch(function () { window.location.href = href; });
  }, true);
})();
`;

export default withDocs(defineConfig({
  vite: {
    plugins: [{
      name: "riftri-public-docs-only",
      enforce: "pre",
      configureServer(server: DevServer) {
        // Farm 0.1.0's dev Markdown routes answer every `*.md` URL, including
        // the module requests Vite makes for the docs pages (`page.md?import`).
        // The failed import stops every page from hydrating in development,
        // so hand those requests to Vite's own transform first.
        server.middlewares.use((req, res, next) => {
          const url = req.url ?? "";
          if (!/\.mdx?\?(?:[^#]*&)?import(?:[&=#]|$)/.test(url)) return next();
          server.transformRequest(url).then((result) => {
            if (!result) return next();
            res.setHeader("Content-Type", "text/javascript");
            res.end(result.code);
          }, next);
        });
      },
      transform(code: string, id: string) {
        if (!id.split("?")[0].replace(/\\/g, "/").endsWith("/@farming-labs/farmjs/dist/react.js")) return null;
        // Adapter 0.2.115 eagerly imports every Markdown file in the project.
        // Only staged public guides belong in the UI; the full-reference `.md`
        // companions under public/docs stay raw assets, not adapter pages.
        const broadGlob = 'import.meta.glob("/**/*.{md,mdx}",';
        if (!code.includes(broadGlob)) throw new Error("Review the docs adapter Markdown glob before upgrading it.");
        const narrowed = code.replace(broadGlob, 'import.meta.glob("/src/app/docs/**/*.{md,mdx}",');
        // The docs adapter is the pages' client hydration entry, so appending a
        // one-time delegated listener here upgrades the injected "Copy .md" link
        // (see stage-website-docs.mjs) into a real clipboard copy without any UI
        // change: it fetches the page's own `.md` and writes it to the clipboard
        // instead of navigating. Without clipboard support the link still opens
        // the Markdown.
        return { code: narrowed + COPY_MARKDOWN_CLIENT, map: null };
      },
    }],
    // Dev pre-bundling would serve the adapter from .vite/deps, where the
    // transform above never sees its real path. The unnarrowed glob then
    // imports Markdown files the dev server cannot serve, and the failed
    // import stops every page from hydrating.
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
}));
