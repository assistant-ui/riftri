import type { LayoutProps, Metadata } from "@farm.js/core";
import "./globals.css";

const siteUrl = "https://riftri.dev";
const title = "Riftri — Lightweight Git workspaces";
const description =
  "Real, isolated Git worktrees for parallel development without eagerly duplicating every unchanged file.";
// Link previews truncate long copy, so social surfaces get a shorter line.
const socialDescription = "Real Git worktrees. Shared unchanged files. Private edits.";
const sharingCardUrl = "https://riftri.dev/og.png";

export const metadata: Metadata = {
  metadataBase: siteUrl,
  title,
  description,
  alternates: {
    canonical: "https://riftri.dev/",
  },
  robots: {
    index: true,
    follow: true,
  },
  openGraph: {
    type: "website",
    url: "https://riftri.dev/",
    siteName: "Riftri",
    title,
    description: socialDescription,
    images: [
      {
        url: sharingCardUrl,
        width: 1200,
        height: 630,
        type: "image/png",
        alt: "Riftri: one immutable base shared by three isolated Git worktrees, each with private edits.",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    title,
    description: socialDescription,
    images: [
      {
        url: sharingCardUrl,
        alt: "Riftri: shared unchanged files, private edits.",
      },
    ],
  },
  icons: {
    icon: [{ url: "/favicon.svg", type: "image/svg+xml", sizes: "any" }],
  },
};

export default function RootLayout({ children }: LayoutProps) {
  return <>{children}</>;
}
