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

function GitHubIcon() {
  return (
    <svg className="github-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82A7.65 7.65 0 0 1 8 3.75c.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
    </svg>
  );
}

function ArrowUpRightIcon() {
  return (
    <svg
      className="external-link-icon"
      viewBox="0 0 16 16"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M4.5 11.5 11.5 4.5M5 4.5h6.5V11" />
    </svg>
  );
}

function GraphLabel({
  index,
  children,
  icon,
}: {
  index: string;
  children: React.ReactNode;
  icon?: React.ReactNode;
}) {
  return (
    <div className="graph-label">
      <span>{index}</span>
      <span className="graph-label-copy">
        <span aria-hidden="true">[</span>
        {icon}
        <span>{children}</span>
        <span aria-hidden="true">]</span>
      </span>
    </div>
  );
}

function Hero() {
  return (
    <section className="hero" id="top">
      <div className="hero-copy">
        <GraphLabel index="00" icon={<GitHubIcon />}>
          OPEN SOURCE / BUILT FOR PARALLEL WORK
        </GraphLabel>
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
            <GitHubIcon /> View GitHub <ArrowUpRightIcon />
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
    description: "Native copy-on-write backends share unchanged data; edits stay private.",
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
          <h2>Install. Check. Create.</h2>
        </div>
        <p>
          Run these commands from a Git repository. Riftri checks the destination first and stops
          unless a supported native storage backend is available.
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
    </section>
  );
}

function Footer() {
  return (
    <footer className="site-footer">
      <span>RIFTRI</span>
      <a href={githubUrl}>
        <GitHubIcon /> GET ON GITHUB <ArrowUpRightIcon />
      </a>
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
