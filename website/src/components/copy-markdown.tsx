"use client";

import { useEffect, useRef, useState } from "react";

type CopyMarkdownProps = {
  /** Same-origin path of the Markdown document to copy, e.g. "/index.md". */
  source: string;
  className?: string;
};

/// Copies the full Markdown guide so it can be pasted straight into an agent
/// conversation. Falls back to opening the document when copying fails.
export function CopyMarkdown({ source, className = "" }: CopyMarkdownProps) {
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
  }, [source]);

  async function copy() {
    const attempt = ++request.current;
    window.clearTimeout(timeout.current);
    setState("idle");
    try {
      const response = await fetch(source, { headers: { Accept: "text/plain" } });
      if (!response.ok) throw new Error(`fetch ${source}: ${response.status}`);
      const markdown = await response.text();
      await navigator.clipboard.writeText(markdown);
      if (attempt !== request.current) return;
      setState("copied");
      timeout.current = window.setTimeout(() => setState("idle"), 1800);
    } catch {
      if (attempt !== request.current) return;
      setState("failed");
    }
  }

  return (
    <>
      <button
        className={`copy-button${copied ? " is-copied" : ""} ${className}`.trim()}
        type="button"
        onClick={copy}
        aria-label={
          state === "failed"
            ? "Copying failed; retry copying the Markdown guide"
            : "Copy the full Markdown guide for your agent"
        }
        title="Copy the full Markdown guide for your agent"
      >
        {copied ? "COPIED" : state === "failed" ? "RETRY" : "COPY .MD"}
      </button>
      <span className={state === "failed" ? "copy-error" : "visually-hidden"} role="status" aria-atomic="true">
        {copied
          ? "Markdown guide copied."
          : state === "failed"
            ? `Copy unavailable. Open ${source} to copy it manually.`
            : ""}
      </span>
    </>
  );
}
