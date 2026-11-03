import type { ReactNode } from "react";

// The docs adapter wraps every docs page in this shell (`withDocs(..., { shell })`
// in farm.config.ts) and imports it from its client hydration entry. Installing
// the listener when the module loads, rather than in an effect, keeps it ready
// before hydration, so an early click copies instead of navigating.
//
// It upgrades the injected "Copy .md" link (see stage-website-docs.mjs) into a
// clipboard copy of the page's own `.md`, falling back to opening the Markdown
// when copying is unavailable. Modified clicks keep the link's normal behavior.
const SELECTOR = 'a[title="Copy this page as Markdown"]';
const timers = new WeakMap<HTMLElement, number>();

declare global {
  interface Window {
    __riftriCopyMarkdown?: boolean;
  }
}

if (typeof document !== "undefined" && !window.__riftriCopyMarkdown) {
  window.__riftriCopyMarkdown = true;
  document.addEventListener("click", (event) => {
    const target = event.target as Element | null;
    const link = target?.closest?.<HTMLAnchorElement>(SELECTOR);
    if (!link || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    if (!navigator.clipboard?.writeText) return;
    event.preventDefault();
    event.stopPropagation();
    const href = link.getAttribute("href")!;
    // Snapshot the original label exactly once so repeated clicks within the
    // restore window can never capture the transient "Copied .md" text and
    // leave the link stuck on it.
    const label = (link.dataset.riftriCopyLabel ??= link.textContent ?? "");
    fetch(href, { headers: { Accept: "text/plain" } })
      .then((response) => {
        if (!response.ok) throw new Error(String(response.status));
        return response.text();
      })
      .then((markdown) => navigator.clipboard.writeText(markdown))
      .then(() => {
        link.textContent = "Copied .md";
        window.clearTimeout(timers.get(link));
        timers.set(link, window.setTimeout(() => { link.textContent = label; }, 1800));
      })
      .catch(() => { window.location.href = href; });
  }, true);
}

export default function DocsShell({ children }: { children: ReactNode }) {
  return <>{children}</>;
}
