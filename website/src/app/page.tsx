import type { Metadata } from "@farm.js/core";
import { CopyCommand } from "../components/copy-command";
import { MaterializationMap } from "../components/materialization-map";
import { SectionLink } from "../components/section-link";
import { StorageMap } from "../components/storage-map";

const githubUrl = "https://github.com/assistant-ui/riftri";

export const metadata: Metadata = {
  title: "Riftri — Lightweight Git workspaces for parallel development",
  description:
    "Create real, isolated Git worktrees without eagerly storing a full physical copy of every unchanged project file.",
};

export const dynamic = "force-static";

function GraphLabel({ index, children }: { index: string; children: React.ReactNode }) {
  return (
    <div className="graph-label">
      <span>{index}</span>
      <span>[ {children} ]</span>
    </div>
  );
}

function Header() {
  return (
    <header className="site-header">
      <SectionLink className="wordmark" href="#top" aria-label="Riftri home">
        <span className="wordmark-mark" aria-hidden="true">r/</span>
        <span>riftri</span>
      </SectionLink>
      <nav className="site-nav" aria-label="Primary navigation">
        <SectionLink href="#overview">Overview</SectionLink>
        <SectionLink href="#start">Get started</SectionLink>
        <SectionLink href="#safety">Safety</SectionLink>
        <a href={githubUrl}>GitHub ↗</a>
      </nav>
    </header>
  );
}

function Hero() {
  return (
    <section className="hero" id="top">
      <div className="hero-copy">
        <GraphLabel index="00">LIGHTWEIGHT GIT WORKSPACES</GraphLabel>
        <h1>
          Parallel work,
          <br />
          without parallel copies.
        </h1>
        <p className="hero-lede">
          Create real, isolated Git worktrees while unchanged files share physical storage.
          Your tools keep using normal files and normal Git.
        </p>
        <div className="hero-actions">
          <SectionLink className="button button-primary" href="#start">
            Get started <span aria-hidden="true">→</span>
          </SectionLink>
          <a className="button button-secondary" href={githubUrl}>
            View GitHub <span aria-hidden="true">↗</span>
          </a>
        </div>
        <p className="micro-note">EXPERIMENTAL / OPTIMIZED OPERATIONS CURRENTLY REQUIRE MACOS + APFS</p>
      </div>
      <div className="hero-graph">
        <StorageMap />
      </div>
    </section>
  );
}

const essentials = [
  {
    index: "01",
    title: "Real Git worktrees",
    description: "Branches, commits, hooks, and status remain owned by Git.",
  },
  {
    index: "02",
    title: "Shared unchanged data",
    description: "APFS clones share existing file blocks; edits stay private to each workspace.",
  },
  {
    index: "03",
    title: "No filesystem proxy",
    description: "Editors, builds, and agents read and write the native filesystem directly.",
  },
] as const;

function Overview() {
  return (
    <section className="content-section" id="overview">
      <div className="section-heading">
        <GraphLabel index="01">WHAT RIFTRI CHANGES</GraphLabel>
        <h2>One exact tree. Many private workspaces.</h2>
        <p>
          Riftri changes how worktree files are created and stored. It does not replace Git,
          manage branches, or sit between your tools and the filesystem.
        </p>
      </div>
      <MaterializationMap />
      <div className="essential-grid">
        {essentials.map((item) => (
          <article key={item.index}>
            <span>{item.index}</span>
            <h3>{item.title}</h3>
            <p>{item.description}</p>
          </article>
        ))}
      </div>
    </section>
  );
}

const startSteps = [
  {
    index: "01",
    title: "Install Riftri",
    description: "The npm launcher installs the matching Rust binary.",
    command: "npm install --global riftri",
  },
  {
    index: "02",
    title: "Check support",
    description: "Doctor is read-only and explains blockers before anything changes.",
    command: "riftri doctor --destination ../app-auth",
  },
  {
    index: "03",
    title: "Create a worktree",
    description: "Start with the explicit, debuggable interface.",
    command: "riftri worktree add ../app-auth -b feature/auth main",
  },
] as const;

function GetStarted() {
  return (
    <section className="content-section start-section" id="start">
      <div className="section-heading split-heading">
        <div>
          <GraphLabel index="02">GET STARTED</GraphLabel>
          <h2>Check first. Create second.</h2>
        </div>
        <p>
          Run these commands from a Git repository on macOS. Riftri will stop with an explanation
          if the destination cannot use its APFS backend safely.
        </p>
      </div>

      <ol className="start-list">
        {startSteps.map((step) => (
          <li key={step.index}>
            <span className="start-index">{step.index}</span>
            <div className="start-copy">
              <strong>{step.title}</strong>
              <p>{step.description}</p>
            </div>
            <CopyCommand command={step.command} label="COPY" compact />
          </li>
        ))}
      </ol>

      <div className="activation-note">
        <div>
          <span>OPTIONAL</span>
          <strong>Prefer normal <code>git worktree</code> commands?</strong>
        </div>
        <p>
          Shell interception is explicitly activated, and <code>riftri enable</code> still opts in
          one repository at a time.
        </p>
        <a href={`${githubUrl}#quick-start-on-macos`}>Read activation setup →</a>
      </div>
    </section>
  );
}

const readyNow = [
  "Real linked worktrees on writable APFS volumes",
  "Journaled add, move, remove, prune, repair, and garbage collection",
  "Changed or ambiguous workspaces are preserved for review",
] as const;

const boundaries = [
  "Optimized mutations are not available on Linux or Windows yet",
  "Git LFS, custom filters, sparse checkout, and submodules are blocked",
  "Riftri is storage optimization, not a security sandbox",
] as const;

function Safety() {
  return (
    <section className="content-section" id="safety">
      <div className="section-heading">
        <GraphLabel index="03">CURRENT BOUNDARY</GraphLabel>
        <h2>Experimental, explicit, and recoverable.</h2>
        <p>
          Riftri fails before mutation when it cannot reproduce Git&apos;s checkout exactly. It never
          silently falls back to a full worktree copy.
        </p>
      </div>

      <div className="boundary-grid">
        <article>
          <span className="boundary-label ready">● READY TO TEST</span>
          <ul>
            {readyNow.map((item) => <li key={item}>{item}</li>)}
          </ul>
        </article>
        <article>
          <span className="boundary-label">○ KNOW BEFORE USE</span>
          <ul>
            {boundaries.map((item) => <li key={item}>{item}</li>)}
          </ul>
        </article>
      </div>

      <div className="warning-note">
        <span>EXPERIMENTAL</span>
        <p>Keep important work committed or backed up before using mutation commands.</p>
        <a href={githubUrl}>View source and full documentation ↗</a>
      </div>
    </section>
  );
}

function Footer() {
  return (
    <footer className="site-footer">
      <span>RIFTRI / EXPERIMENTAL OPEN SOURCE</span>
      <span>LIGHTWEIGHT GIT WORKSPACES</span>
      <a href={githubUrl}>ASSISTANT-UI/RIFTRI ↗</a>
    </footer>
  );
}

export default function HomePage() {
  return (
    <div className="site-shell">
      <div className="announcement">
        <span>Experimental</span>
        <p>APFS-backed worktrees are ready for real-world testing.</p>
        <SectionLink href="#safety">Current support →</SectionLink>
      </div>
      <div className="page-frame">
        <Header />
        <main>
          <Hero />
          <Overview />
          <GetStarted />
          <Safety />
        </main>
        <Footer />
      </div>
    </div>
  );
}
