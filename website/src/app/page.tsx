import type { Metadata } from "@farm.js/core";
import { CopyCommand } from "../components/copy-command";
import { MaterializationMap } from "../components/materialization-map";
import { SectionLink } from "../components/section-link";
import { StorageMap } from "../components/storage-map";

const githubUrl = "https://github.com/assistant-ui/riftri";
const agentbaseUrl = "https://agentbase.dev";
const installCommand = "curl -fsSL https://riftri.vercel.app/install.sh | bash";

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
        <div className="hero-install">
          <CopyCommand command={installCommand} compact />
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
        <GraphLabel index="01">STORAGE MODEL</GraphLabel>
        <h2>How Riftri stores linked worktrees</h2>
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

const benchmarkStats = [
  {
    index: "01",
    value: "87%",
    unit: "less new disk allocation",
    description:
      "Creating all ten workspaces allocated 100.6 MiB where ordinary Git worktrees allocated 774.0 MiB for the same trees.",
  },
  {
    index: "02",
    value: "2.46 MiB",
    unit: "per additional workspace",
    description:
      "After the first workspace primes the shared immutable base, each further workspace allocated about 2.46 MiB instead of a full ~77 MiB copy of the checkout.",
  },
  {
    index: "03",
    value: "0.25 MiB",
    unit: "growth after ten agents worked",
    description:
      "Ten coding agents then edited and tested inside their own views; measured volume growth stayed within a quarter of a mebibyte because only changed blocks are written.",
  },
] as const;

function Benchmark() {
  return (
    <section className="content-section bench-section" id="benchmark">
      <div className="section-heading split-heading">
        <div>
          <GraphLabel index="02">MEASURED SAVINGS / ASSISTANT-UI</GraphLabel>
          <h2>Ten parallel workspaces, about one checkout of disk</h2>
        </div>
        <p>
          A recorded experiment created ten isolated worktrees of assistant-ui — a real
          public repository with 5,346 tracked files and roughly 60.6 MiB of tracked
          content — once with ordinary Git and once with Riftri on macOS/APFS, measuring
          new volume allocation for each.
        </p>
      </div>

      <figure className="bench-figure" aria-label="New disk allocation for ten worktrees">
        <div className="bench-row">
          <span className="bench-row-label">ORDINARY GIT WORKTREES</span>
          <div className="bench-track">
            <div className="bench-bar bench-bar-git" style={{ width: "100%" }} />
            <span className="bench-value">774.0 MiB</span>
          </div>
        </div>
        <div className="bench-row">
          <span className="bench-row-label">RIFTRI WORKTREES</span>
          <div className="bench-track">
            <div className="bench-bar bench-bar-riftri" style={{ width: "13%" }} />
            <span className="bench-value">100.6 MiB</span>
          </div>
        </div>
        <figcaption>
          New volume allocation while creating ten worktrees of the same tree, smaller is
          better.
        </figcaption>
      </figure>

      <div className="essential-grid bench-grid">
        {benchmarkStats.map((stat) => (
          <article key={stat.index}>
            <span>{stat.index}</span>
            <p className="bench-stat">
              <strong>{stat.value}</strong> {stat.unit}
            </p>
            <p>{stat.description}</p>
          </article>
        ))}
      </div>

      <p className="bench-footnote">
        One local experiment, not a universal promise: Riftri was slower to create the
        views (22.4 s versus 9.9 s for all ten), and results vary with filesystem,
        file count, and later private writes. Method, raw numbers, and caveats:{" "}
        <a href={`${githubUrl}/blob/main/docs/benchmarks/assistant-ui-ten-agents-2026-09-12.md`}>
          the full write-up
        </a>
        .
      </p>
    </section>
  );
}

const startSteps = [
  {
    index: "01",
    title: "Install on macOS or Linux",
    description: "Downloads the native CLI and verifies SHA-256. No Node.js required.",
    command: installCommand,
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
          <GraphLabel index="03">QUICK START</GraphLabel>
          <h2>Create a Riftri worktree</h2>
        </div>
        <p>
          Install the CLI, then run the support check from a Git repository. Riftri stops
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
      <div className="install-notes">
        <p>
          Installs to <code>~/.local/bin</code>. Follow the printed PATH command before step 02.
          Shell profiles and Git activation stay unchanged. Linux supports glibc and musl.
        </p>
        <div className="install-links">
          <a href="/install.sh">Read the Bash installer</a>
          <a href="/install.ps1">Read the PowerShell installer</a>
          <a href={`${githubUrl}/blob/main/docs/install.md#windows-powershell`}>Windows &amp; manual install</a>
          <a href={`${githubUrl}/releases`}>Release downloads</a>
        </div>
      </div>
    </section>
  );
}

function Footer() {
  return (
    <footer className="site-footer">
      <a className="footer-maker-link" href={agentbaseUrl}>
        AGENTBASE AI <ArrowUpRightIcon />
      </a>
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
          <Benchmark />
          <GetStarted />
        </main>
        <Footer />
      </div>
    </div>
  );
}
