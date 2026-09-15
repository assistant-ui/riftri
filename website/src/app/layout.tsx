import type { LayoutProps, Metadata } from "@farm.js/core";
import "./globals.css";

export const metadata: Metadata = {
  title: "Riftri — Lightweight Git workspaces",
  description:
    "Real, isolated Git worktrees for parallel development without eagerly duplicating every unchanged file.",
  metadataBase: "https://riftri.dev",
  alternates: { canonical: "https://riftri.dev/" },
  openGraph: {
    type: "website",
    url: "https://riftri.dev/",
    siteName: "Riftri",
    title: "Riftri — Lightweight Git workspaces",
    description: "Real Git worktrees. Shared unchanged files. Private edits.",
    images: [{
      url: "https://riftri.dev/og.png",
      width: 1200,
      height: 630,
      type: "image/png",
      alt: "Riftri: one immutable base shared by three isolated Git worktrees, each with private edits.",
    }],
  },
  twitter: {
    card: "summary_large_image",
    title: "Riftri — Lightweight Git workspaces",
    description: "Real Git worktrees. Shared unchanged files. Private edits.",
    images: [{ url: "https://riftri.dev/og.png", alt: "Riftri: shared unchanged files, private edits." }],
  },
  icons: {
    icon: [{ url: "/favicon.svg", type: "image/svg+xml", sizes: "any" }],
  },
};

export default function RootLayout({ children }: LayoutProps) {
  return <>{children}</>;
}
