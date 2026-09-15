"use client";

import { useState, useSyncExternalStore } from "react";

const backends = ["APFS", "Linux reflink", "ReFS"];

function subscribeReducedMotion(onChange: () => void) {
  const query = window.matchMedia("(prefers-reduced-motion: reduce)");
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

const getReducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const getServerReducedMotion = () => true;

export function SavingsBackendName() {
  const [paused, setPaused] = useState(false);
  const reducedMotion = useSyncExternalStore(subscribeReducedMotion, getReducedMotion, getServerReducedMotion);
  const action = `${paused ? "Resume" : "Pause"} backend name animation`;

  return (
    <span className="savings-backend-label">
      Riftri /{" "}
      <button
        type="button"
        className={`savings-backend-toggle${paused ? " is-paused" : ""}`}
        onClick={() => setPaused((value) => !value)}
        disabled={reducedMotion}
        aria-pressed={paused}
        aria-label={reducedMotion ? "Supported storage backends" : action}
        aria-describedby="savings-backends"
        title={reducedMotion ? "Animation disabled by reduced-motion preference" : action}
      >
        <span className="savings-backend-cycle" aria-hidden="true">
          {backends.map((backend) => <span className="savings-backend-item" key={backend}>{backend}</span>)}
        </span>
      </button>
      <span className="visually-hidden" id="savings-backends">
        APFS on macOS, native reflinks on Linux, and ReFS on Windows.
        The chart shows the APFS reference measurement.
      </span>
    </span>
  );
}
