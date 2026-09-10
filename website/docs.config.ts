import type { FarmDocsSidebarItem } from "@farm.js/core";
import { defineDocs } from "@farming-labs/docs";

type RiftriDocsConfig = Parameters<typeof defineDocs>[0] & {
  favicon?: string;
  navigation?: {
    sidebar?: FarmDocsSidebarItem[];
  };
};

const sidebar = [
  {
    label: "Start",
    children: [
      { label: "Overview", slug: "" },
      { label: "Getting started", slug: "getting-started" },
    ],
  },
  {
    label: "Concepts",
    children: [
      { label: "How Riftri works", slug: "how-it-works" },
      { label: "Git interception", slug: "git-interception" },
      { label: "Storage and cleanup", slug: "storage-and-cleanup" },
    ],
  },
  {
    label: "Reference",
    children: [
      { label: "Compatibility", slug: "compatibility" },
      { label: "CLI commands", slug: "commands" },
    ],
  },
] satisfies FarmDocsSidebarItem[];

const config = {
  entry: "docs",
  docsPath: "/docs",
  metadata: {
    description: "Install, use, and understand Riftri's lightweight Git workspaces.",
  },
  favicon: "/favicon.svg",
  nav: {
    title: "Riftri",
    url: "/",
  },
  github: {
    url: "https://github.com/assistant-ui/riftri",
    directory: "website",
  },
  search: {
    provider: "simple",
    enabled: true,
    maxResults: 12,
  },
  llmsTxt: {
    enabled: true,
    siteTitle: "Riftri Docs",
    siteDescription: "Documentation for lightweight Git workspaces for parallel development.",
  },
  sitemap: true,
  robots: true,
  breadcrumb: {
    enabled: true,
  },
  lastUpdated: {
    enabled: true,
    label: "Last updated",
    position: "footer",
  },
  pageActions: {
    position: "below-title",
    copyMarkdown: {
      enabled: true,
    },
    openDocs: {
      enabled: true,
      target: "markdown",
    },
    alignment: "right",
  },
  themeToggle: {
    enabled: true,
    default: "dark",
  },
  navigation: {
    sidebar,
  },
} satisfies RiftriDocsConfig;

export default {
  ...defineDocs(config),
  favicon: config.favicon,
  navigation: config.navigation,
} satisfies RiftriDocsConfig;
