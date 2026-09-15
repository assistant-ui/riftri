"use client";

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import data from "../data/space-savings.json";

const toMiB = (bytes: number) => (bytes / 2 ** 20).toFixed(2);
const savedPercent = (1 - data.riftriBytes / data.gitBytes) * 100;
const platforms = [
  { id: "macos", name: "macOS", backend: "APFS", environment: "macOS ARM64 · APFS", detail: "Native APFS clones" },
  { id: "linux", name: "Linux", backend: "reflink", environment: "Linux · Btrfs / XFS", detail: "Native reflinks on Btrfs and reflink-enabled XFS. OverlayFS is available where its active probe succeeds." },
  { id: "windows", name: "Windows", backend: "ReFS", environment: "Windows · ReFS", detail: "Native ReFS block clones. Ordinary NTFS volumes are not supported." },
] as const;

function subscribeReducedMotion(onChange: () => void) {
  const query = window.matchMedia("(prefers-reduced-motion: reduce)");
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

const getReducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const getServerReducedMotion = () => true;

export function SavingsMap() {
  const [selected, setSelected] = useState(0);
  const [paused, setPaused] = useState(false);
  const [inView, setInView] = useState(false);
  const figure = useRef<HTMLElement>(null);
  const reducedMotion = useSyncExternalStore(subscribeReducedMotion, getReducedMotion, getServerReducedMotion);

  useEffect(() => {
    if (!figure.current) return;
    const observer = new IntersectionObserver(([entry]) => setInView(entry.isIntersecting), { threshold: 0.2 });
    observer.observe(figure.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (paused || reducedMotion || !inView) return;
    const interval = window.setInterval(() => {
      if (!document.hidden) setSelected((index) => (index + 1) % platforms.length);
    }, 7000);
    return () => window.clearInterval(interval);
  }, [paused, reducedMotion, inView]);

  return (
    <figure
      className="savings-map graph-frame"
      aria-describedby="savings-scope"
      ref={figure}
    >
      <span className="corner corner-tl" aria-hidden="true">+</span>
      <span className="corner corner-tr" aria-hidden="true">+</span>
      <span className="corner corner-bl" aria-hidden="true">+</span>
      <span className="corner corner-br" aria-hidden="true">+</span>
      <figcaption className="frame-title">[ ASSISTANT-UI / DISK ALLOCATION ]</figcaption>

      <div className="savings-controls">
        <div className="savings-platforms" role="group" aria-label="Benchmark platform" onFocusCapture={() => setPaused(true)}>
          {platforms.map((platform, index) => (
            <button
              type="button"
              key={platform.id}
              aria-pressed={selected === index}
              aria-controls={`savings-${platform.id}`}
              onClick={() => { setSelected(index); setPaused(true); }}
            >
              {platform.name}
            </button>
          ))}
        </div>
        <button
          type="button"
          className="savings-cycle-toggle"
          disabled={reducedMotion}
          onClick={() => setPaused((value) => !value)}
        >
          {reducedMotion ? "Motion off" : paused ? "Resume cycle" : "Pause cycle"}
        </button>
      </div>

      <div className="savings-panels">
        {platforms.map((platform, index) => {
          const measured = platform.id === data.platform;
          const active = selected === index;
          const lanes = [
            { name: "Ordinary Git", bytes: measured ? data.gitBytes : null, detail: measured ? "10 full checkouts" : "No recorded run", className: "" },
            { name: `Riftri / ${platform.backend}`, bytes: measured ? data.riftriBytes : null, detail: measured ? "10 views + shared base" : platform.name, className: "savings-lane-riftri" },
          ];

          return (
            <div
              className={`savings-panel${active ? " is-active" : ""}`}
              id={`savings-${platform.id}`}
              key={platform.id}
              aria-hidden={!active}
              inert={!active}
            >
              <div className="savings-meta">
                <span>{measured ? `${data.worktrees} WORKTREES / ${data.filesPerView.toLocaleString("en-US")} FILES EACH` : "ASSISTANT-UI / NO RECORDED RUN"}</span>
                <span>{platform.environment}{measured ? ` · v${data.version}` : ""}</span>
              </div>
              <div className="savings-comparison">
                <div className="savings-chart">
                  <p className="savings-chart-label">
                    {measured ? "New volume allocation after creation · lower is better" : `No assistant-ui allocation measurement recorded on ${platform.name}.`}
                  </p>
                  <dl className="savings-lanes">
                    {lanes.map((lane) => (
                      <div className={`savings-lane ${lane.className}`} key={lane.name}>
                        <dt>{lane.name}<small>{lane.detail}</small></dt>
                        <dd>
                          {lane.bytes === null ? <strong className="savings-unmeasured">Not measured</strong> : (
                            <strong>{toMiB(lane.bytes)} <span>MiB</span></strong>
                          )}
                          <span className="savings-bar" aria-hidden="true">
                            <span>[</span>
                            <span className={`savings-track${measured ? "" : " savings-track-unmeasured"}`}>
                              {lane.bytes !== null ? (
                                <span
                                  className="savings-fill"
                                  style={{ clipPath: `inset(0 ${(1 - lane.bytes / data.gitBytes) * 100}% 0 0)` }}
                                />
                              ) : null}
                            </span>
                            <span>]</span>
                          </span>
                        </dd>
                      </div>
                    ))}
                  </dl>
                  <div className="savings-scale" aria-hidden="true">
                    {measured ? <><span>0 MiB</span><span>{toMiB(data.gitBytes)} MiB</span></> : <span>No comparison available</span>}
                  </div>
                </div>
                <div className="savings-result">
                  <span className="savings-result-label">{measured ? "LESS NEW ALLOCATION" : `${platform.name.toUpperCase()} / ${platform.backend.toUpperCase()}`}</span>
                  {measured ? (
                    <>
                      <strong>{savedPercent.toFixed(1)}<span>%</span></strong>
                      <p>{toMiB(data.gitBytes - data.riftriBytes)} MiB saved</p>
                      <small>Includes the shared base and all ten views.</small>
                    </>
                  ) : (
                    <>
                      <strong className="savings-pending">Not measured</strong>
                      <small>{platform.detail}</small>
                      <small>The APFS result does not measure savings on {platform.name}.</small>
                    </>
                  )}
                </div>
              </div>
            </div>
          );
        })}
      </div>

      <div className="savings-notes" id="savings-scope">
        <p>
          <strong>APFS timing: not a speed claim.</strong> Creation was slower in this run:
          {" "}{data.riftriSeconds.toFixed(2)} s with Riftri vs {data.gitSeconds.toFixed(2)} s with Git.
        </p>
        <p>
          Historical experiment · <time dateTime={data.date}>12 Sep 2026</time> · adjusted
          assistant-ui source fixture, with the <code>linguist-generated</code> display hint removed.
          Tracked source only; dependencies and full builds excluded. Measured at the volume
          level, not by summing file sizes. Results vary by workload and filesystem.
        </p>
        <a href={`https://github.com/assistant-ui/riftri/blob/main/${data.reportPath}`} onFocus={() => setPaused(true)}>
          Read the benchmark &amp; methodology <span aria-hidden="true">↗</span>
        </a>
      </div>
    </figure>
  );
}
