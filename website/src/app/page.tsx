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

function Hero() {
  return (
    <section className="hero" id="top">
      <div className="hero-copy">
        <GraphLabel index="00">OPEN SOURCE / BUILT FOR PARALLEL WORK</GraphLabel>
        <h1>Riftri</h1>
        <p className="hero-kicker">LIGHTWEIGHT GIT WORKSPACES FOR PARALLEL DEVELOPMENT_</p>
        <p className="hero-lede">
          Real, isolated Git worktrees that share the unchanged parts of your project.
          Keep using normal files, normal Git, and the tools you already have.
        </p>
        <div className="hero-actions">
          <SectionLink className="button button-primary" href="#start">
            Get started <span aria-hidden="true">→</span>
          </SectionLink>
          <a className="button button-secondary" href={githubUrl}>
            View GitHub <span aria-hidden="true">↗</span>
          </a>
        </div>
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
        <h2>One tree. Many workspaces.</h2>
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
          <h2>Three commands. Then work.</h2>
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

      <div className="release-note">
        <span>EXPERIMENTAL</span>
        <p>Optimized operations currently require macOS and APFS. Unsupported checkouts stop before mutation.</p>
        <a href={githubUrl}>Full support notes ↗</a>
      </div>
    </section>
  );
}

function Footer() {
  return (
    <footer className="site-footer">
      <span>RIFTRI</span>
      <a href={githubUrl}>ASSISTANT-UI/RIFTRI ↗</a>
    </footer>
  );
}

export default function HomePage() {
  return (
    <div className="site-shell">
      <div className="page-frame">
        <main>
          <Hero />
          <Overview />
          <GetStarted />
        </main>
        <Footer />
      </div>
    </div>
  );
}
