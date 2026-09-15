"use client";

import { useEffect, useState } from "react";

type CopyCommandProps = {
  command: string;
  label?: string;
  compact?: boolean;
  multiline?: boolean;
  prompt?: string;
};

export function CopyCommand({ command, label = "COPY", compact = false, multiline = false, prompt = "$" }: CopyCommandProps) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const copied = state === "copied";

  useEffect(() => {
    if (!copied) return;
    const timeout = window.setTimeout(() => setState("idle"), 1800);
    return () => window.clearTimeout(timeout);
  }, [copied]);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setState("copied");
    } catch {
      setState("failed");
    }
  }

  return (
    <div className={`command ${compact ? "command-compact" : ""}${multiline ? " command-multiline" : ""}`} aria-label={`Command: ${command}`}>
      <span className="command-prompt" aria-hidden="true">{prompt}</span>
      <code>{command}</code>
      <button className={`copy-button${copied ? " is-copied" : ""}`} type="button" onClick={copy} aria-label={`${copied ? "Copied" : "Copy"} command: ${command}`} aria-live="polite">
        {copied ? "COPIED" : state === "failed" ? "RETRY" : label}
      </button>
      {state === "failed" ? (
        <span className="copy-error" role="status">Copy unavailable. Select the command to copy it manually.</span>
      ) : null}
    </div>
  );
}
