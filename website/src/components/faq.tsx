const githubDocs = "https://github.com/assistant-ui/riftri/blob/main/docs";

const questions = [
  {
    question: "Why use Riftri if I can ask an agent to make a COW copy?",
    answer: (
      <>
        <p>
          A copy-on-write (COW) copy can work well if you already handle Git setup
          and cleanup yourself.
          Riftri gives you a repeatable way to create real Git worktrees, reuse a
          verified base from an exact Git tree, check storage support, and recover
          interrupted operations. You get those checks every time, without relying
          on an agent to remember each step.
        </p>
        <p>
          For a small project or an occasional copy, your existing approach may be
          enough. Riftri is useful when you create and retire worktrees regularly.
        </p>
      </>
    ),
  },
  {
    question: "Are these real Git worktrees?",
    answer: (
      <p>
        Yes. Git registers each worktree with its own HEAD and index, while repository
        history is shared. Your usual Git commands, editor, and build tools still work.
        Riftri handles how the checkout files are stored; ordinary reads and writes
        go directly through the native filesystem.
      </p>
    ),
  },
  {
    question: "What disk space does it actually save?",
    answer: (
      <p>
        Compatible worktrees starting from the same Git tree share the unchanged
        tracked file data through one immutable base. Edits allocate private storage.
        Dependencies such as <code>node_modules</code>, build output, caches, and Git
        metadata still take space. Savings depend on the workload and filesystem;
        summing directory sizes can count shared blocks more than once. See the{" "}
        <a href={`${githubDocs}/benchmarks.md`}>measurements and their limits</a>.
      </p>
    ),
  },
  {
    question: "Is it faster than ordinary Git?",
    answer: (
      <p>
        Not always. The first worktree needs a base to be built, and cached worktrees
        still go through integrity and Git checks. Our published APFS experiment used
        much less additional disk space but took longer to create the worktrees.
        Expect storage savings on compatible workloads, not a guaranteed speedup.
      </p>
    ),
  },
  {
    question: "Does my coding agent need special integration?",
    answer: (
      <p>
        You can create a worktree with Riftri and point any agent at its directory.
        For normal Git command interception, run <code>riftri enable</code> in the
        repository, then launch your agent with <code>riftri exec -- claude</code>
        (or another command). Tools that call Git through PATH inherit the shim.
        Tools using their own Git library or an absolute Git path bypass it. See{" "}
        <a href={`${githubDocs}/agent-integration.md`}>agent setup</a>.
      </p>
    ),
  },
  {
    question: "Does enabling Riftri change Git everywhere?",
    answer: (
      <p>
        No. <code>riftri enable</code> opts in one repository. Interception also needs
        an activated shell hook or <code>riftri exec</code>. Even a hook in your shell
        profile leaves other repositories on ordinary Git, and existing worktrees
        are not converted. Use <code>riftri shell status</code> to check the scope and{" "}
        <code>riftri disable</code> to opt the repository out. See{" "}
        <a href={`${githubDocs}/global-activation.md`}>activation and deactivation</a>.
      </p>
    ),
  },
  {
    question: "Will it work on my machine and repository?",
    answer: (
      <>
        <p>
          Optimized worktrees need APFS on macOS, Btrfs or reflink-enabled XFS on Linux,
          or ReFS on Windows. Linux can also use OverlayFS when mount permissions or
          Riftri’s explicitly installed helper allow it. Ordinary NTFS is not supported.
        </p>
        <p>
          Run <code>riftri doctor --destination ../app-auth</code> from your repository
          first. Some checkout settings are unsupported, and Riftri stops with an
          explanation instead of silently making a full copy. It is still experimental;
          keep important work committed or backed up.
        </p>
      </>
    ),
  },
  {
    question: "What happens after a crash, and how do I clean up?",
    answer: (
      <>
        <p>
          Riftri records lifecycle progress in durable journals. <code>riftri repair</code>{" "}
          uses them to recover interrupted operations and preserves changed or
          ambiguous state for inspection. <code>riftri status</code> shows what is retained.
        </p>
        <p>
          Use <code>riftri worktree remove &lt;path&gt;</code> for a finished, clean worktree.
          Its base stays cached for reuse. <code>riftri gc</code> previews unused-base
          cleanup; only <code>riftri gc --apply</code> deletes eligible bases. Cleanup
          is explicit, and dirty work is not automatically discarded.
        </p>
      </>
    ),
  },
  {
    question: "Can one agent’s edits affect another worktree?",
    answer: (
      <p>
        Writes through one worktree’s files stay private; they do not update the
        shared base or a sibling worktree. That isolation is not a security sandbox.
        An agent with permission to access another directory can still edit it directly,
        and Git branches and history belong to the shared repository. Use a container
        or VM when you need to isolate untrusted code.
      </p>
    ),
  },
];

export function Faq() {
  return (
    <div className="faq-list">
      {questions.map(({ question, answer }, index) => (
        <details className="faq-item" key={question} open={index === 0}>
          <summary>
            <span className="faq-question">
              <span className="faq-prompt" aria-hidden="true">&gt;</span>
              <span>{question}</span>
            </span>
            <span className="faq-toggle" aria-hidden="true">
              <span className="faq-plus">[+]</span>
              <span className="faq-minus">[−]</span>
            </span>
          </summary>
          <div className="faq-answer">{answer}</div>
        </details>
      ))}
    </div>
  );
}
