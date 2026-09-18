import type { Metadata } from "@farm.js/core";
import { CopyCommand } from "../components/copy-command";
import { Faq } from "../components/faq";
import { MaterializationMap } from "../components/materialization-map";
import { SavingsMap } from "../components/savings-map";
import { SectionLink } from "../components/section-link";
import { StorageMap } from "../components/storage-map";

const githubUrl = "https://github.com/assistant-ui/riftri";
const agentbaseUrl = "https://agentbase.dev";
const installCommand = "curl -fsSL https://riftri.dev/install.sh | bash";
const powershellInstall = `$Installer = Join-Path $env:TEMP 'riftri-install.ps1'
Invoke-WebRequest https://riftri.dev/install.ps1 -OutFile $Installer
Get-Content $Installer`;

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

function Header() {
  return (
    <header className="site-header">
      <SectionLink className="site-brand" href="#top" aria-label="Riftri home">
        <img src="/favicon.svg" width="28" height="28" alt="" />
        Riftri
      </SectionLink>
      <nav aria-label="Main navigation">
        <SectionLink href="#overview">How it works</SectionLink>
        <SectionLink href="#savings">Savings</SectionLink>
        <SectionLink href="#faq">FAQ</SectionLink>
        <a href={`${githubUrl}/blob/main/docs/README.md`}>Docs <ArrowUpRightIcon /></a>
      </nav>
      <a className="header-github" href={githubUrl} aria-label="Riftri on GitHub">
        <GitHubIcon /> <span>GitHub</span> <ArrowUpRightIcon />
      </a>
    </header>
  );
}

function Hero() {
  return (
    <section className="hero" id="top" tabIndex={-1} aria-labelledby="top-title">
      <div className="hero-copy">
        <GraphLabel index="00" icon={<GitHubIcon />}>
          OPEN SOURCE / NATIVE COPY-ON-WRITE
        </GraphLabel>
        <h1 id="top-title">Git worktrees.<br /><span>Shared storage.</span></h1>
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
          <a
            className="copy-button hero-markdown"
            href="https://riftri.dev/index.md"
            type="text/plain"
            aria-label="Open Markdown guide"
            title="Open Markdown guide"
          >
            .md
          </a>
        </div>
      </div>
      <div className="hero-graph">
        <p className="hero-graph-label"><span>STORAGE LAYOUT</span><span aria-hidden="true">FIG. 01</span></p>
        <StorageMap />
        <p className="hero-graph-caption">One base. Independent worktrees. Only edits diverge.</p>
      </div>
    </section>
  );
}

function StorageBackends() {
  return (
    <div className="storage-backends">
      <p>Native storage <span>on supported volumes</span></p>
      <ul aria-label="Storage backends by platform">
        <li><span>macOS</span> APFS</li>
        <li><span>Linux</span> reflink / OverlayFS</li>
        <li><span>Windows</span> ReFS</li>
      </ul>
    </div>
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
    <section className="content-section" id="overview" tabIndex={-1} aria-labelledby="overview-title">
      <div className="section-heading">
        <GraphLabel index="01">STORAGE MODEL</GraphLabel>
        <h2 id="overview-title">How Riftri stores linked worktrees</h2>
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

function Savings() {
  return (
    <section className="content-section" id="savings" tabIndex={-1} aria-labelledby="savings-title">
      <div className="section-heading split-heading">
        <div>
          <GraphLabel index="02">MEASURED SAVINGS</GraphLabel>
          <h2 id="savings-title">Worktree disk usage</h2>
        </div>
        <p>
          New disk allocation for ten assistant-ui worktrees, using the same
          tracked source in both comparisons.
        </p>
      </div>
      <SavingsMap />
    </section>
  );
}

const startSteps = [
  {
    index: "01",
    title: "Install the CLI",
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
    <section className="content-section start-section" id="start" tabIndex={-1} aria-labelledby="start-title">
      <div className="section-heading split-heading">
        <div>
          <GraphLabel index="03">QUICK START</GraphLabel>
          <h2 id="start-title">Create a Riftri worktree</h2>
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
            {step.index === "01" ? (
              <div className="install-options">
                <p className="install-platform">macOS / Linux</p>
                <CopyCommand command={step.command} label="COPY" compact />
                <details className="windows-install">
                  <summary>Windows / PowerShell</summary>
                  <p>Download the installer and review its contents.</p>
                  <CopyCommand command={powershellInstall} compact multiline prompt="PS" />
                  <p>After reviewing, run it in the same PowerShell session:</p>
                  <CopyCommand command="& $Installer" compact prompt="PS" />
                  <p>
                    Installs per user; follow its printed PATH command. Optimized worktrees
                    require a ReFS volume, not ordinary NTFS. No administrator access is
                    needed to install the CLI.
                  </p>
                </details>
              </div>
            ) : <CopyCommand command={step.command} label="COPY" compact />}
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
      <a href="https://riftri.dev/index.md" rel="alternate" type="text/plain">
        READ MARKDOWN
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
      <SectionLink className="skip-link" href="#main-content">Skip to content</SectionLink>
      <div className="page-frame">
        <Header />
        <main id="main-content" tabIndex={-1}>
          <Hero />
          <StorageBackends />
          <Overview />
          <Savings />
          <GetStarted />
          <section className="content-section faq-section" id="faq" tabIndex={-1} aria-labelledby="faq-title">
            <div className="section-heading">
              <GraphLabel index="04">FAQ</GraphLabel>
              <h2 id="faq-title">Common questions</h2>
              <p>Where Riftri fits, what it saves, and what stays in your control.</p>
              <a className="button button-secondary" href={`${githubUrl}/issues`}>
                Ask a question <ArrowUpRightIcon />
              </a>
            </div>
            <Faq />
          </section>
        </main>
        <Footer />
      </div>
    </div>
  );
}
