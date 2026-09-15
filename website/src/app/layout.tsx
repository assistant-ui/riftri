import type { LayoutProps, Metadata } from "@farm.js/core";
import "./globals.css";

const siteUrl = "https://riftri.dev";
const title = "Riftri — Lightweight Git workspaces";
const description =
  "Real, isolated Git worktrees for parallel development without eagerly duplicating every unchanged file.";

export const metadata: Metadata = {
  metadataBase: siteUrl,
  title,
  description,
  alternates: {
    canonical: siteUrl,
  },
  robots: {
    index: true,
    follow: true,
  },
  openGraph: {
    type: "website",
    url: siteUrl,
    siteName: "Riftri",
    title,
    description,
  },
  // Summary rather than summary_large_image: the only brand asset is an SVG
  // favicon, and social crawlers do not render SVG previews.
  twitter: {
    card: "summary",
    title,
    description,
  },
  icons: {
    icon: [{ url: "/favicon.svg", type: "image/svg+xml", sizes: "any" }],
  },
};

export default function RootLayout({ children }: LayoutProps) {
  return <>{children}</>;
}
