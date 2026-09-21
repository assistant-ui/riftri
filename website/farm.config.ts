import { defineConfig } from "@farm.js/core";
import { withDocs } from "@farming-labs/farmjs/config";

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
      transform(code: string, id: string) {
        if (!id.split("?")[0].replace(/\\/g, "/").endsWith("/@farming-labs/farmjs/dist/react.js")) return null;
        // Adapter 0.2.115 eagerly imports every Markdown file in the project.
        // Only staged public guides belong in the UI; agent.md stays a raw asset.
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
