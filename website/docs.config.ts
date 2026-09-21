import { defineDocs } from "@farming-labs/docs";
import { pixelBorder } from "@farming-labs/theme/pixel-border";
import groups from "./content/docs.json";
import icons from "./content/docs-icons.json";

export default {
  ...defineDocs({
    entry: "docs",
    contentDir: "src/app/docs",
    theme: pixelBorder({
      ui: {
        colors: {
          primary: "#f06a3a",
          background: "#0a0a0a",
          muted: "#101010",
          mutedForeground: "#a7a7a2",
          border: "#292929",
        },
        typography: {
          font: {
            h1: { size: "2rem", weight: 500, letterSpacing: "-0.035em" },
            h2: { size: "1.375rem", weight: 500, letterSpacing: "-0.025em" },
            h3: { size: "1.0625rem", weight: 500 },
            body: { size: "0.9375rem", weight: 380 },
          },
        },
        layout: { sidebarWidth: 264, contentWidth: 720 },
      },
    }),
    nav: { title: "Riftri", url: "/" },
    themeToggle: { enabled: false, default: "dark" },
    sidebar: { flat: true, collapsible: false },
    metadata: {
      titleTemplate: "%s — Riftri",
      description: "Install, use, and understand Riftri copy-on-write Git worktrees.",
    },
    search: { enabled: true, provider: "simple" },
    ai: { enabled: false },
    mcp: false,
    telemetry: false,
    lastUpdated: false,
    // Page actions are hand-injected Markdown links ("View .md / Copy .md")
    // below each page's intro; the framework's native button is disabled so the
    // two links can share one row.
    pageActions: { copyMarkdown: false },
    // /index.md remains the public, hand-maintained agent overview.
    llmsTxt: false,
    sitemap: { enabled: false },
  }),
  favicon: "/favicon.svg",
  icons,
  navigation: { sidebar: groups.map((group) => ({
    label: group.label,
    children: group.pages.map((page) => ({ label: page.title, icon: page.icon, href: `/docs${page.slug ? `/${page.slug}` : ""}` })),
  })) },
};
