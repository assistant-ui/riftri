"use client";

import { useEffect, useRef, useState } from "react";
import { easeOut, fixed, flat, lens, roundedRect, smooth, solid, span, type Point } from "./figure-kit";

const stages = [
  { index: "01", title: "Exact Git tree", detail: "a4d2c19" },
  { index: "02", title: "Immutable base", detail: "reuse or create" },
  { index: "03", title: "Native COW view", detail: "platform backend" },
  { index: "04", title: "Linked worktree", detail: "clean + writable" },
] as const;

const platforms = [
  { name: "macOS", backend: "APFS clone", status: "current" },
  { name: "Linux", backend: "reflink / OverlayFS", status: "experimental · current" },
  { name: "Windows", backend: "ReFS block clone", status: "experimental · current" },
] as const;

function BackendCycle() {
  return (
    <>
      <span className="backend-cycle" aria-hidden="true">
        {platforms.map((platform) => (
          <span className="backend-cycle-item" key={platform.name}>
            <strong>{platform.backend}</strong>
            <small>{platform.name} · {platform.status}</small>
          </span>
        ))}
      </span>
      <span className="visually-hidden">
        Native copy-on-write view. APFS clone on macOS, Linux reflinks and mount-capable OverlayFS,
        and Windows ReFS block clones are current experimental backends.
      </span>
    </>
  );
}

const WIDTH = 200;
const HEIGHT = 112;
const view = lens(Math.PI / 4, Math.PI / 6, 17, 100, 78);
const GRID = 4;
const TILE = 1;
const GAP = 0.22;
const EXTENT = (GRID * TILE + (GRID - 1) * GAP) / 2;
const HALF = EXTENT + 0.3;
const TILE_TOP = 0.5;
const LIFT = 2.4;

const tileStart = (index: number) => -EXTENT + index * (TILE + GAP);
const tiles = Array.from({ length: GRID * GRID }, (_, index) => [index % GRID, Math.floor(index / GRID)] as const)
  .sort(([a, b], [c, d]) => a + b - (c + d));
const tileOutline = ([column, row]: readonly [number, number]) =>
  roundedRect(tileStart(column), tileStart(row), tileStart(column) + TILE, tileStart(row) + TILE, 0.14);
const center = ([column, row]: readonly [number, number], z: number): Point =>
  view.at(tileStart(column) + TILE / 2, tileStart(row) + TILE / 2, z);
const plateOutline = roundedRect(-HALF, -HALF, HALF, HALF, 0.32);
const basePlate = solid(view, plateOutline, 0, 0.22);
const baseTiles = tiles.map((tile) => solid(view, tileOutline(tile), 0.22, TILE_TOP));
const groundPlate = flat(view, plateOutline, 0);

// One pass through the four stages per backend name, on the backend name's own
// CSS animation clock: 9s for three names, so a pass every 3s.
const PASS = 3000;
const CYCLE = 9000;

/** How far the current pass has reached each stage: 0 before it, rising to 1 once it is done. */
function reach(phase: number, stage: number) {
  return easeOut(span(phase, 0.1 + stage * 0.2, 0.22));
}

function useCyclePhase(track: React.RefObject<HTMLOListElement | null>) {
  // The first pass's start pose is what the server renders.
  const [phase, setPhase] = useState(0);

  useEffect(() => {
    const node = track.current;
    if (!node) return;
    const motion = matchMedia("(prefers-reduced-motion: reduce)");
    let frame = 0;
    let visible = false;

    const tick = () => {
      const animation = node.querySelector(".backend-cycle-item")?.getAnimations()[0];
      const timing = animation?.effect?.getTiming();
      if (!animation || animation.currentTime == null || !timing) {
        setPhase(1);
        return;
      }
      const into = (Number(animation.currentTime) - Number(timing.delay ?? 0) + CYCLE) % CYCLE;
      setPhase((into % PASS) / PASS);
      frame = requestAnimationFrame(tick);
    };
    const sync = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      // Reduced motion shows every stage finished.
      if (motion.matches) setPhase(1);
      else if (visible) frame = requestAnimationFrame(tick);
    };
    const observer = new IntersectionObserver(([entry]) => {
      visible = entry.isIntersecting;
      sync();
    });
    observer.observe(node);
    motion.addEventListener("change", sync);
    return () => {
      observer.disconnect();
      motion.removeEventListener("change", sync);
      cancelAnimationFrame(frame);
    };
  }, [track]);

  return phase;
}

function TreeFigure({ progress }: { progress: number }) {
  // The tree is only a list of entries: dots on the ground, nothing stored yet.
  return (
    <>
      <path className="fig-view" d={groundPlate} />
      {tiles.map((tile, index) => {
        const [x, y] = center(tile, 0);
        const lit = progress > (index + 1) / (tiles.length + 1);
        return <circle className={`fig-entry${lit ? " is-lit" : ""}`} cx={fixed(x)} cy={fixed(y)} r="1.6" key={index} />;
      })}
    </>
  );
}

function BaseFigure({ progress }: { progress: number }) {
  const lit = progress > 0 && progress < 1;
  return (
    <>
      <path className="fig-face fig-base-plate" d={basePlate.side} />
      <path className="fig-face fig-base-plate" d={basePlate.top} />
      {baseTiles.map((tile, index) => (
        <g className={`fig-base-tile${lit ? " is-lit" : ""}`} key={index}>
          <path className="fig-face fig-side" d={tile.side} />
          <path className="fig-face" d={tile.top} />
        </g>
      ))}
    </>
  );
}

function ViewFigure({ progress }: { progress: number }) {
  const z = TILE_TOP + (LIFT - TILE_TOP) * progress;
  return (
    <>
      <BaseFigure progress={0} />
      <g style={{ opacity: fixed(Math.min(1, progress * 3)) }}>
        <path className={`fig-view${progress > 0 && progress < 1 ? " is-lit" : ""}`} d={flat(view, plateOutline, z)} />
        {tiles.map((tile, index) => {
          const [x, y] = center(tile, z);
          return <circle className="fig-shared" cx={fixed(x)} cy={fixed(y)} r="1.1" key={index} />;
        })}
      </g>
    </>
  );
}

function WorktreeFigure({ progress }: { progress: number }) {
  const [x, y] = view.at(HALF, -HALF, 0);
  return (
    <>
      <path className={`fig-worktree${progress > 0 ? " is-lit" : ""}`} d={groundPlate} />
      {tiles.map((tile, index) => {
        const [cx, cy] = center(tile, 0);
        return <circle className="fig-shared" cx={fixed(cx)} cy={fixed(cy)} r="1.1" key={index} />;
      })}
      <circle className={`fig-status${progress > 0.6 ? " is-on" : ""}`} cx={fixed(x + 9)} cy={fixed(y)} r="3" />
    </>
  );
}

const figures = [TreeFigure, BaseFigure, ViewFigure, WorktreeFigure];

export function MaterializationTrack() {
  const track = useRef<HTMLOListElement>(null);
  const phase = useCyclePhase(track);
  // Settle the finished stages back at the end of each pass, before the next one starts.
  const settle = 1 - smooth(span(phase, 0.93, 0.07));

  return (
    <ol className="materialization-track" aria-label="Riftri worktree materialization path" ref={track}>
      {stages.map((stage, index) => {
        const Figure = figures[index];
        const progress = phase >= 1 ? 1 : reach(phase, index) * settle;
        return (
          <li className={stage.index === "03" ? "is-backend-stage" : undefined} key={stage.index}>
            <svg className="stage-figure" viewBox={`0 0 ${WIDTH} ${HEIGHT}`} aria-hidden="true" focusable="false">
              <Figure progress={progress} />
            </svg>
            <span>{stage.index}</span>
            {stage.index === "03" ? (
              <BackendCycle />
            ) : (
              <>
                <strong>{stage.title}</strong>
                <small>{stage.detail}</small>
              </>
            )}
          </li>
        );
      })}
    </ol>
  );
}
