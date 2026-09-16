"use client";

import { useState, useSyncExternalStore, type ReactNode } from "react";

function subscribe(onChange: () => void) {
  const query = window.matchMedia("(prefers-reduced-motion: reduce)");
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

const getSnapshot = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const getServerSnapshot = () => true;

export function MotionFigure({ className, label, note, children }: {
  className: string;
  label: string;
  note: string;
  children: ReactNode;
}) {
  const [paused, setPaused] = useState(false);
  const reducedMotion = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);

  return (
    <figure className={`${className} motion-figure`} data-paused={paused}>
      {children}
      <div className="diagram-controls">
        <span>{note}</span>
        <button
          type="button"
          aria-label={reducedMotion ? `${label} motion disabled by preference` : `Pause ${label} animation`}
          aria-pressed={paused}
          disabled={reducedMotion}
          onClick={() => setPaused((value) => !value)}
        >
          {reducedMotion ? "Motion off" : paused ? "Resume" : "Pause"}
        </button>
      </div>
    </figure>
  );
}
