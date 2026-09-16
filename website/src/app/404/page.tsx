import type { Metadata } from "@farm.js/core";

const githubUrl = "https://github.com/assistant-ui/riftri";

export const metadata: Metadata = {
  title: "Page not found — Riftri",
  description: "That page does not exist on riftri.dev.",
  robots: {
    index: false,
    follow: true,
  },
};

export const dynamic = "force-static";

export default function NotFoundPage() {
  return (
    <div className="site-shell">
      <div className="page-frame">
        <main>
          <section className="hero">
            <div className="hero-copy">
              <h1>404</h1>
              <p className="hero-kicker">THAT PAGE DOES NOT EXIST_</p>
              <p className="hero-lede">
                The installer lives at <code>/install.sh</code>, and the full guide is
                available as Markdown at <code>/index.md</code>. Everything else is on
                GitHub.
              </p>
              <div className="hero-actions">
                <a className="button button-primary" href="/">
                  Back to the homepage <span aria-hidden="true">→</span>
                </a>
                <a className="button button-secondary" href={githubUrl}>
                  View GitHub
                </a>
              </div>
            </div>
          </section>
        </main>
      </div>
    </div>
  );
}
