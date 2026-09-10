import type { Metadata } from "@farm.js/core";
import { CopyCommand } from "../components/copy-command";
import { StorageMap } from "../components/storage-map";

const githubUrl = "https://github.com/assistant-ui/riftri";

export const metadata: Metadata = {
  title: "Riftri — Lightweight Git workspaces for parallel development",
  description:
    "Create real, isolated Git worktrees without eagerly storing a full physical copy of every unchanged project file.",
};

function Wordmark() {
  return (
    <span className="wordmark" aria-label="Riftri">
      <span aria-hidden="true" className="wordmark-mark">
        r/
      </span>
      <span>riftri</span>
    </span>
  );
}

function Header() {
  return (
    <header className="site-header">
      <a className="brand-link" href="/" aria-label="Riftri home">
        <Wordmark />
      </a>
      <nav className="site-nav" aria-label="Primary navigation">
        <a href="#how-it-works">How it works</a>
        <a href="/docs">Docs</a>
        <a href={githubUrl}>GitHub ↗</a>
      </nav>
    </header>
  );
}

function Hero() {
  return (
    <section className="hero">
      <div className="hero-copy">
        <div className="section-label">
          <span>00</span>
          <span>Parallel development / less duplication</span>
        </div>
        <h1>
          Parallel work,
          <br />
          without parallel copies.
        </h1>
        <p className="hero-lede">
          Riftri gives every developer or coding agent a real, isolated Git worktree while unchanged
          files share physical storage.
        </p>
        <div className="hero-actions">
          <a className="button button-primary" href="/docs/getting-started">
            Get started <span aria-hidden="true">→</span>
          </a>
          <a className="button button-secondary" href={githubUrl}>
            View source <span aria-hidden="true">↗</span>
          </a>
        </div>
        <CopyCommand command="npm install --global riftri" />
        <p className="pre-release-note">Experimental v0.1 · optimized operations require macOS + APFS</p>
      </div>
      <div className="hero-visual">
        <StorageMap />
      </div>
    </section>
  );
}

const steps = [
  {
    index: "01",
    title: "Resolve the tree",
    text: "Git remains the source of truth. Riftri resolves the exact commit, tree, and checkout profile.",
    code: "git rev-parse main^{tree}",
  },
  {
    index: "02",
    title: "Reuse one base",
    text: "A read-only base is materialized once and reused only when repository, tree, profile, and volume match.",
    code: "base / exact-tree / APFS",
  },
  {
    index: "03",
    title: "Clone private views",
    text: "Native APFS clones share existing blocks. Each worktree allocates private blocks as files change.",
    code: "clonefile(base, worktree)",
  },
] as const;

function HowItWorks() {
  return (
    <section className="section" id="how-it-works">
      <div className="section-intro">
        <div className="section-label">
          <span>01</span>
          <span>How it works</span>
        </div>
        <h2>Git semantics in front. Native storage underneath.</h2>
        <p>
          Riftri only participates when worktrees are created, moved, removed, or repaired. Editors,
          builds, and agents read and write the native filesystem directly.
        </p>
      </div>
      <div className="step-grid">
        {steps.map((step) => (
          <article className="step" key={step.index}>
            <span className="step-index">{step.index}</span>
            <h3>{step.title}</h3>
            <p>{step.text}</p>
            <code>{step.code}</code>
          </article>
        ))}
      </div>
    </section>
  );
}

function Workflow() {
  return (
    <section className="section workflow-section">
      <div className="workflow-copy">
        <div className="section-label">
          <span>02</span>
          <span>Normal Git workflow</span>
        </div>
        <h2>Enable once. Keep using Git.</h2>
        <p>
          Add the shell hook when you want transparent interception. Optimization still requires
          explicit consent inside each repository, so unrelated repositories continue to use normal
          Git.
        </p>
        <a className="text-link" href="/docs/git-interception">
          Understand interception <span aria-hidden="true">→</span>
        </a>
      </div>
      <div className="terminal" aria-label="Terminal example">
        <div className="terminal-header">
          <span>RIFTRI / SESSION</span>
          <span className="terminal-status">● APFS ready</span>
        </div>
        <pre>
          <code>
            <span className="terminal-muted"># activate the shim in this shell</span>{"\n"}
            <span className="terminal-prompt">$</span> eval &quot;$(riftri shell hook zsh)&quot;{"\n\n"}
            <span className="terminal-muted"># opt this repository in</span>{"\n"}
            <span className="terminal-prompt">$</span> riftri enable{"\n"}
            <span className="terminal-output">Enabled for this repository</span>{"\n\n"}
            <span className="terminal-prompt">$</span> git worktree add -b feature/auth ../app-auth main{"\n"}
            <span className="terminal-output">Created APFS-backed Git worktree</span>
          </code>
        </pre>
      </div>
    </section>
  );
}

const proof = [
  ["REAL", "Git linked worktrees", "Branches, commits, hooks, and ordinary commands stay Git-owned."],
  ["NATIVE", "No filesystem proxy", "There is no daemon in the read/write path after creation."],
  ["SAFE", "No silent full copy", "Unsupported checkouts stop before Riftri creates managed state."],
] as const;

function Proof() {
  return (
    <section className="section proof-section">
      <div className="proof-metric">
        <div className="section-label">
          <span>03</span>
          <span>Recorded APFS check</span>
        </div>
        <strong>0.146%</strong>
        <p>
          Volume growth for a cached view with a 32 MiB tracked payload in one recorded development
          run. Results vary with filesystem activity and later edits.
        </p>
        <a className="text-link" href="/docs/storage-and-cleanup#measuring-disk-use">
          See the measurement <span aria-hidden="true">→</span>
        </a>
      </div>
      <div className="proof-list">
        {proof.map(([label, title, text]) => (
          <article key={label}>
            <span>{label}</span>
            <h3>{title}</h3>
            <p>{text}</p>
          </article>
        ))}
      </div>
    </section>
  );
}

function Compatibility() {
  return (
    <section className="section compatibility-section">
      <div className="section-intro compact">
        <div className="section-label">
          <span>04</span>
          <span>Compatibility</span>
        </div>
        <h2>Experimental by design. Fail-closed by default.</h2>
        <p>
          The APFS backend is ready to test on real projects. Linux and Windows CLIs provide
          diagnostics today; their mutation backends remain on the roadmap.
        </p>
      </div>
      <div className="compat-table" role="table" aria-label="Platform compatibility">
        <div className="compat-row compat-head" role="row">
          <span role="columnheader">Platform</span>
          <span role="columnheader">Backend</span>
          <span role="columnheader">Status</span>
        </div>
        <div className="compat-row" role="row">
          <strong role="cell">macOS</strong>
          <span role="cell">APFS native clones</span>
          <span className="status status-ready" role="cell">
            ● Ready to test
          </span>
        </div>
        <div className="compat-row" role="row">
          <strong role="cell">Linux</strong>
          <span role="cell">Reflink / OverlayFS</span>
          <span className="status" role="cell">
            ○ Planned next
          </span>
        </div>
        <div className="compat-row" role="row">
          <strong role="cell">Windows</strong>
          <span role="cell">ReFS</span>
          <span className="status" role="cell">
            ○ Roadmap
          </span>
        </div>
      </div>
    </section>
  );
}

function FinalCta() {
  return (
    <section className="final-cta">
      <div>
        <div className="section-label">
          <span>05</span>
          <span>Start with a diagnostic</span>
        </div>
        <h2>Know before Riftri changes anything.</h2>
        <p>
          Run <code>riftri doctor</code> to inspect Git and the destination filesystem without
          creating a worktree or managed state.
        </p>
      </div>
      <a className="button button-primary" href="/docs/getting-started">
        Read the guide <span aria-hidden="true">→</span>
      </a>
    </section>
  );
}

function Footer() {
  return (
    <footer className="site-footer">
      <Wordmark />
      <p>Lightweight Git workspaces for parallel development.</p>
      <div>
        <a href="/docs">Docs</a>
        <a href={githubUrl}>GitHub ↗</a>
        <span>Apache-2.0</span>
      </div>
    </footer>
  );
}

export default function HomePage() {
  return (
    <div className="site-shell">
      <div className="announcement">
        <span>Experimental</span>
        <p>APFS-backed worktrees are ready for real-world testing.</p>
        <a href="/docs/compatibility">Current support →</a>
      </div>
      <div className="page-frame">
        <Header />
        <main>
          <Hero />
          <HowItWorks />
          <Workflow />
          <Proof />
          <Compatibility />
          <FinalCta />
        </main>
        <Footer />
      </div>
    </div>
  );
}
