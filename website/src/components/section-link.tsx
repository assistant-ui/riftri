"use client";

import { useEffect, type ComponentProps, type MouseEvent } from "react";

type SectionLinkProps = Omit<ComponentProps<"a">, "href"> & {
  href: `#${string}`;
};

function scrollToSection(hash: string) {
  const target = document.getElementById(hash.slice(1));
  if (!target) return false;

  const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  target.scrollIntoView({ behavior: reduceMotion ? "auto" : "smooth", block: "start" });
  return true;
}

export function SectionLink({ href, onClick, ...props }: SectionLinkProps) {
  useEffect(() => {
    if (window.location.hash !== href) return;
    const frame = window.requestAnimationFrame(() => scrollToSection(href));
    return () => window.cancelAnimationFrame(frame);
  }, [href]);

  function handleClick(event: MouseEvent<HTMLAnchorElement>) {
    onClick?.(event);
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
      return;
    }
    if (!scrollToSection(href)) return;
    event.preventDefault();
    window.history.pushState(null, "", href);
  }

  return <a href={href} onClick={handleClick} {...props} />;
}
