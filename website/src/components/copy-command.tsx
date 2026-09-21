"use client";

import { useEffect, useRef, useState } from "react";

type CopyCommandProps = {
  command: string;
  label?: string;
  compact?: boolean;
  multiline?: boolean;
  prompt?: string;
};

export function CopyCommand({ command, label = "COPY", compact = false, multiline = false, prompt = "$" }: CopyCommandProps) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const timeout = useRef<number | undefined>(undefined);
  const request = useRef(0);
  const copied = state === "copied";

  useEffect(() => {
    setState("idle");
    return () => {
      request.current += 1;
      window.clearTimeout(timeout.current);
    };
  }, [command]);

  async function copy() {
    const attempt = ++request.current;
    window.clearTimeout(timeout.current);
    setState("idle");
    try {
      await navigator.clipboard.writeText(command);
      if (attempt !== request.current) return;
      setState("copied");
      timeout.current = window.setTimeout(() => setState("idle"), 1800);
    } catch {
      if (attempt !== request.current) return;
      setState("failed");
    }
  }

  return (
    <div className={`command ${compact ? "command-compact" : ""}${multiline ? " command-multiline" : ""}`}>
      <span className="command-prompt" aria-hidden="true">{prompt}</span>
      <code>{command}</code>
      <button className={`copy-button${copied ? " is-copied" : ""}`} type="button" onClick={copy} aria-label={`${state === "failed" ? "Retry copying" : "Copy"} command: ${command}`}>
        {copied ? "COPIED" : state === "failed" ? "RETRY" : label}
      </button>
      <span className={state === "failed" ? "copy-error" : "visually-hidden"} role="status" aria-atomic="true">
        {copied ? "Command copied." : state === "failed" ? "Copy unavailable. Select the command to copy it manually." : ""}
      </span>
    </div>
  );
}
