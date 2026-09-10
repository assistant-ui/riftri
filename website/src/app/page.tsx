import type { Metadata } from "@farm.js/core";
import { CopyCommand } from "../components/copy-command";
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

function CornerMarks() {
  return (
    <>
      <span className="corner corner-tl" aria-hidden="true">+</span>
      <span className="corner corner-tr" aria-hidden="true">+</span>
      <span className="corner corner-bl" aria-hidden="true">+</span>
      <span className="corner corner-br" aria-hidden="true">+</span>
    </>
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
        <SectionLink href="#model">Model</SectionLink>
        <SectionLink href="#workflow">Workflow</SectionLink>
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
          Riftri gives every developer or coding agent a real, isolated Git worktree while unchanged
          files share physical storage.
        </p>
        <div className="hero-actions">
          <SectionLink className="button button-primary" href="#workflow">
            Try the workflow <span aria-hidden="true">→</span>
          </SectionLink>
          <a className="button button-secondary" href={githubUrl}>
            View source <span aria-hidden="true">↗</span>
          </a>
        </div>
        <CopyCommand command="npm install --global riftri" label="INSTALL" />
        <p className="micro-note">EXPERIMENTAL V0.1 / OPTIMIZED OPERATIONS REQUIRE MACOS + APFS</p>
      </div>
      <div className="hero-graph">
        <StorageMap />
      </div>
    </section>
  );
}

const metrics = [
  { value: "01", label: "immutable base", meter: "████████████████" },
  { value: "03", label: "isolated views", meter: "████████████░░░░" },
  { value: "0.146%", label: "recorded allocation", meter: "█░░░░░░░░░░░░░░░" },
] as const;

function Metrics() {
  return (
    <section className="metric-strip" aria-label="Storage model summary">
      {metrics.map((metric) => (
        <article className="metric" key={metric.label}>
          <span className="metric-value">{metric.value}</span>
          <div>
            <span className="metric-label">{metric.label}</span>
            <span className="metric-meter" aria-hidden="true">{metric.meter}</span>
          </div>
        </article>
      ))}
    </section>
  );
}

function Model() {
  return (
    <section className="content-section" id="model">
      <div className="section-heading">
        <GraphLabel index="01">THE STORAGE MODEL</GraphLabel>
        <h2>One exact tree. Many private views.</h2>
        <p>
          Ordinary Git worktrees share repository objects, but each checked-out file is materialized
          again. Riftri adds a reusable immutable base and lets APFS share those file blocks.
        </p>
      </div>

      <div className="compare-grid">
        <article className="graph-frame compare-card">
          <CornerMarks />
          <span className="frame-title">[ ORDINARY WORKTREES ]</span>
          <div className="tree-lines" aria-label="Ordinary worktree storage model">
            <div><span>repo</span><span className="tree-meta">git objects</span></div>
            <p>├── main/ <span>████████████████</span></p>
            <p>├── auth/ <span>████████████████</span></p>
            <p>└── tests/ <span>████████████████</span></p>
          </div>
          <p className="card-caption">Each checkout materializes another complete set of files.</p>
        </article>

        <article className="graph-frame compare-card compare-card-accent">
          <CornerMarks />
          <span className="frame-title">[ RIFTRI WORKSPACES ]</span>
          <div className="tree-lines" aria-label="Riftri shared storage model">
            <div><span>base</span><span className="tree-meta">████████████████</span></div>
            <p>├── main/ <span>··············▓█</span></p>
            <p>├── auth/ <span>·············▓██</span></p>
            <p>└── tests/ <span>···············█</span></p>
          </div>
          <p className="card-caption">Unchanged blocks stay shared. Only edits become private.</p>
        </article>
      </div>
    </section>
  );
}

const flowSteps = [
  { index: "01", title: "Resolve Git intent", detail: "main → tree a4d2c19", state: "exact tree" },
  { index: "02", title: "Preflight", detail: "profile + APFS capability", state: "validated" },
  { index: "03", title: "Lock base", detail: "repo / tree / profile / volume", state: "reuse or create" },
  { index: "04", title: "Clone private view", detail: "immutable base → app-auth", state: "shared blocks" },
  { index: "05", title: "Link + verify", detail: ".git metadata + clean status", state: "committed" },
] as const;

function ProcessFlow() {
  return (
    <section className="content-section process-section" id="transaction">
      <div className="section-heading split-heading">
        <div>
          <GraphLabel index="02">CREATION TRANSACTION</GraphLabel>
          <h2>Git in front. Native storage underneath.</h2>
        </div>
        <p>
          Riftri participates during lifecycle operations, then gets out of the way. Editors, builds,
          and agents read and write the normal filesystem directly.
        </p>
      </div>
      <div className="graph-frame flow-frame">
        <CornerMarks />
        <span className="frame-title">[ WORKTREE ADD / HAPPY PATH ]</span>
        <div className="transaction-head">
          <code><span aria-hidden="true">$</span> riftri worktree add ../app-auth -b feature/auth main</code>
          <span className="transaction-state"><i aria-hidden="true" /> JOURNAL / ADD</span>
        </div>
        <ol className="flow-track" aria-label="Riftri worktree creation transaction">
          {flowSteps.map(({ index, title, detail, state }) => (
            <li className="flow-step" key={index}>
              <span className="flow-index">STEP {index}</span>
              <span className="flow-node" aria-hidden="true"><i /></span>
              <strong>{title}</strong>
              <small>{detail}</small>
              <span className="flow-state">{state}</span>
            </li>
          ))}
        </ol>
        <div className="transaction-foot">
          <span><b>ON FAILURE</b> rollback known steps · retain ambiguous views</span>
          <strong><i aria-hidden="true" /> RESULT / CLEAN LINKED WORKTREE</strong>
        </div>
      </div>
    </section>
  );
}

function Workflow() {
  return (
    <section className="content-section" id="workflow">
      <div className="section-heading split-heading">
        <div>
          <GraphLabel index="03">START HERE</GraphLabel>
          <h2>Check first. Create second.</h2>
        </div>
        <p>
          Start with the explicit command. Transparent interception is optional and still requires
          repository-local consent.
        </p>
      </div>

      <div className="workflow-grid">
        <article className="graph-frame command-card">
          <CornerMarks />
          <span className="frame-title">[ 01 / DIAGNOSE ]</span>
          <p>Read-only capability check for the repository and destination volume.</p>
          <CopyCommand command="riftri doctor --destination ../app-auth" label="COPY" compact />
          <div className="command-result"><span>✓</span> no files or Git metadata changed</div>
        </article>

        <article className="graph-frame command-card">
          <CornerMarks />
          <span className="frame-title">[ 02 / CREATE ]</span>
          <p>Create a real linked worktree from the exact Git tree.</p>
          <CopyCommand command="riftri worktree add ../app-auth -b feature/auth main" label="COPY" compact />
          <div className="command-result"><span>✓</span> clean APFS-backed worktree</div>
        </article>
      </div>

      <article className="graph-frame terminal-frame">
        <CornerMarks />
        <span className="frame-title">[ OPTIONAL / TRANSPARENT GIT ]</span>
        <div className="terminal-head">
          <span>SESSION: ZSH</span>
          <span className="live-status">● APFS READY</span>
        </div>
        <pre><code><span className="muted"># make interception available in this shell</span>{"\n"}<span className="prompt">$</span> eval &quot;$(riftri shell hook zsh)&quot;{"\n\n"}<span className="muted"># consent is still local to this repository</span>{"\n"}<span className="prompt">$</span> riftri enable{"\n"}<span className="output">✓ enabled for this repository</span>{"\n\n"}<span className="prompt">$</span> git worktree add -b feature/billing ../app-billing main{"\n"}<span className="output">✓ created APFS-backed Git worktree</span></code></pre>
        <div className="terminal-foot">
          <span>NORMAL GIT COMMANDS → REAL GIT</span>
          <span>SUPPORTED WORKTREE OPS → RIFTRI</span>
        </div>
      </article>
    </section>
  );
}

const guarantees = [
  ["[x]", "Real Git linked worktrees", "Branches, commits, hooks, and status stay Git-owned."],
  ["[x]", "No read/write proxy", "No daemon or mount sits between your tools and files."],
  ["[x]", "No silent full copy", "Unsupported checkouts stop before managed state is created."],
  ["[x]", "Repository-local consent", "A global shell hook never opts unrelated repositories in."],
  ["[x]", "Recoverable lifecycle", "Add, move, remove, prune, and GC use durable journals."],
  ["[x]", "Changed views survive", "Repair preserves ambiguous or modified worktrees for review."],
] as const;

function Safety() {
  return (
    <section className="content-section" id="safety">
      <div className="section-heading">
        <GraphLabel index="04">SAFETY CONTRACT</GraphLabel>
        <h2>Fail closed. Explain what remains.</h2>
        <p>
          Riftri treats storage changes as recoverable transactions and never guesses that a changed
          workspace is safe to delete.
        </p>
      </div>

      <div className="safety-grid">
        <article className="graph-frame checklist-frame">
          <CornerMarks />
          <span className="frame-title">[ GUARANTEES ]</span>
          <ul>
            {guarantees.map(([mark, title, description]) => (
              <li key={title}>
                <span className="check-mark">{mark}</span>
                <div><strong>{title}</strong><p>{description}</p></div>
              </li>
            ))}
          </ul>
        </article>

        <article className="graph-frame status-frame">
          <CornerMarks />
          <span className="frame-title">[ RIFTRI STATUS ]</span>
          <div className="status-block">
            <div><span>repository</span><strong>assistant-ui/app</strong></div>
            <div><span>backend</span><strong className="accent-text">apfs / available</strong></div>
            <div><span>retained bases</span><strong>1</strong></div>
            <div><span>active views</span><strong>3</strong></div>
            <div><span>incomplete journals</span><strong>0</strong></div>
            <div><span>unexplained state</span><strong>0</strong></div>
          </div>
          <div className="ascii-divider" aria-hidden="true">+------------------------------+</div>
          <p className="status-note">
            <span>INFO</span> <code>riftri status</code> reports state and disk accounting. It never deletes anything.
          </p>
          <div className="status-actions">
            <code>riftri repair</code><span>recover known journals</span>
            <code>riftri gc --apply</code><span>collect zero-reference bases</span>
          </div>
        </article>
      </div>
    </section>
  );
}

const platforms = [
  ["macOS", "APFS native clones", "READY TO TEST", "ready"],
  ["Linux", "reflink / OverlayFS", "PLANNED NEXT", "planned"],
  ["Windows", "ReFS block cloning", "ROADMAP", "planned"],
] as const;

function Compatibility() {
  return (
    <section className="content-section compatibility-section" id="compatibility">
      <div className="section-heading split-heading">
        <div>
          <GraphLabel index="05">COMPATIBILITY</GraphLabel>
          <h2>Experimental, with a narrow boundary.</h2>
        </div>
        <p>
          Optimized mutations currently require macOS and a writable APFS destination. Linux and
          Windows builds provide diagnostics and explicit unsupported-backend errors.
        </p>
      </div>

      <div className="graph-frame platform-frame">
        <CornerMarks />
        <span className="frame-title">[ BACKEND MATRIX ]</span>
        <table>
          <thead><tr><th>Platform</th><th>Native backend</th><th>Status</th></tr></thead>
          <tbody>
            {platforms.map(([platform, backend, status, kind]) => (
              <tr key={platform}>
                <th scope="row">{platform}</th>
                <td>{backend}</td>
                <td><span className={`platform-status ${kind}`}>{kind === "ready" ? "●" : "○"} {status}</span></td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="compat-note">
        <span>[ BLOCKED CHECKOUTS ]</span>
        <p>Git LFS · custom filters · sparse checkout · submodules · external attributes</p>
        <p>Riftri stops before mutation when it cannot reproduce Git&apos;s checkout exactly.</p>
      </div>
    </section>
  );
}

function FinalCallout() {
  return (
    <section className="final-callout">
      <div>
        <GraphLabel index="06">TRY IT ON A REAL REPOSITORY</GraphLabel>
        <h2>Start with a diagnostic.</h2>
        <p><code>riftri doctor</code> tells you whether the repository and destination are safe before Riftri changes anything.</p>
      </div>
      <div className="final-actions">
        <CopyCommand command="riftri doctor --destination ../app-next" label="COPY" compact />
        <a className="button button-primary" href={githubUrl}>Open GitHub <span aria-hidden="true">↗</span></a>
      </div>
    </section>
  );
}

function Footer() {
  return (
    <footer className="site-footer">
      <span>RIFTRI / EXPERIMENTAL OPEN SOURCE</span>
      <span>LIGHTWEIGHT GIT WORKSPACES FOR PARALLEL DEVELOPMENT</span>
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
        <SectionLink href="#compatibility">Current support →</SectionLink>
      </div>
      <div className="page-frame">
        <Header />
        <main>
          <Hero />
          <Metrics />
          <Model />
          <ProcessFlow />
          <Workflow />
          <Safety />
          <Compatibility />
          <FinalCallout />
        </main>
        <Footer />
      </div>
    </div>
  );
}
