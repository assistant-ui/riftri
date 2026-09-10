"use client";

import { useEffect, useState } from "react";

type CopyCommandProps = {
  command: string;
  label?: string;
  compact?: boolean;
};

export function CopyCommand({ command, label = "COPY", compact = false }: CopyCommandProps) {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timeout = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(timeout);
  }, [copied]);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className={`command ${compact ? "command-compact" : ""}`} aria-label={`Command: ${command}`}>
      <span className="command-prompt" aria-hidden="true">$</span>
      <code>{command}</code>
      <button className="copy-button" type="button" onClick={copy} aria-live="polite">
        {copied ? "COPIED" : label}
      </button>
    </div>
  );
}
