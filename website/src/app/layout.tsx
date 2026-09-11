import type { LayoutProps, Metadata } from "@farm.js/core";
import "./globals.css";

export const metadata: Metadata = {
  title: "Riftri — Lightweight Git workspaces",
  description:
    "Real, isolated Git worktrees for parallel development without eagerly duplicating every unchanged file.",
  icons: {
    icon: [{ url: "/favicon.svg", type: "image/svg+xml", sizes: "any" }],
  },
};

export default function RootLayout({ children }: LayoutProps) {
  return <>{children}</>;
}
