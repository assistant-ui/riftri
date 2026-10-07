"use client";

import { useRef, useState, type PointerEvent } from "react";
import {
  easeOut,
  fixed,
  flat,
  lens,
  roundedRect,
  smooth,
  solid,
  span,
  usePlayhead,
  type Point,
} from "./figure-kit";

const WIDTH = 600;
const HEIGHT = 430;
const view = lens(Math.PI / 4, Math.PI / 6, 38, 175, 318);

const GRID = 4;
const TILE = 1;
const GAP = 0.22;
const EXTENT = (GRID * TILE + (GRID - 1) * GAP) / 2;
const HALF = EXTENT + 0.3;
const BASE_TOP = 0.22;
const TILE_TOP = 0.5;
const BLOCK = 0.3;
const LABEL_X = 372;

// One loop: the worktrees lift off the base, edits copy single blocks up into
// them, everything holds, then it all settles back onto the base.
const PERIOD = 12;
// Server render, first paint, and reduced motion all show the held pose.
const REST = 6.4;
const RESET = 9.6;

const worktrees = [
  { index: "01", name: "auth/", branch: "feature/auth", z: 2.9 },
  { index: "02", name: "billing/", branch: "feature/billing", z: 4.6 },
  { index: "03", name: "tests/", branch: "fix/worktree-tests", z: 6.3 },
] as const;

// Tiles are picked so no block overlaps the base or another block on screen.
const edits = [
  { worktree: 0, tile: [0, 3], at: 2.4 },
  { worktree: 1, tile: [0, 2], at: 2.85 },
  { worktree: 0, tile: [3, 0], at: 3.3 },
  { worktree: 2, tile: [3, 3], at: 3.75 },
  { worktree: 0, tile: [1, 1], at: 4.2 },
  { worktree: 1, tile: [3, 1], at: 4.65 },
] as const;

const tileStart = (index: number) => -EXTENT + index * (TILE + GAP);
const tileOutline = ([column, row]: readonly [number, number]) =>
  roundedRect(tileStart(column), tileStart(row), tileStart(column) + TILE, tileStart(row) + TILE, 0.14);
const tileDepth = ([column, row]: readonly [number, number]) =>
  view.depth(tileStart(column) + TILE / 2, tileStart(row) + TILE / 2);

const tiles = Array.from({ length: GRID * GRID }, (_, index) => [index % GRID, Math.floor(index / GRID)] as const)
  .sort((left, right) => tileDepth(left) - tileDepth(right));
const plateOutline = roundedRect(-HALF, -HALF, HALF, HALF, 0.32);

// The base never moves, so it is drawn once.
const basePlate = solid(view, plateOutline, 0, BASE_TOP);
const baseTiles = tiles.map((tile) => solid(view, tileOutline(tile), BASE_TOP, TILE_TOP));

/** The plate corner nearest the label column, where each wire starts. */
const corner = (z: number): Point => view.at(HALF, -HALF, z);

function lift(worktree: number, t: number) {
  const up = easeOut(span(t, 0.3 + 0.55 * worktree, 0.9));
  const down = smooth(span(t, RESET + 0.6 + 0.25 * (worktrees.length - 1 - worktree), 0.8));
  return up * (1 - down);
}

function wire(worktree: number, t: number) {
  return easeOut(span(t, 0.95 + 0.55 * worktree, 0.45)) * (1 - smooth(span(t, RESET + 0.2, 0.35)));
}

function edit(index: number, t: number) {
  const { at } = edits[index];
  const reset = smooth(span(t, RESET + 0.05 * index, 0.45));
  return {
    travel: smooth(span(t, at, 0.7)) * (1 - reset),
    grow: easeOut(span(t, at + 0.6, 0.4)) * (1 - reset),
    // The newest block keeps the bright edge until the next edit starts.
    fresh: t >= at && t < (edits[index + 1]?.at ?? at + 1.2) && reset === 0,
    resetting: reset > 0,
  };
}

type Focus = number | "base" | null;

export function StorageFigure() {
  const stage = useRef<HTMLDivElement>(null);
  const [focus, setFocus] = useState<Focus>(null);
  const t = usePlayhead(stage, { period: PERIOD, rest: REST, hold: focus !== null });

  const heights = worktrees.map((worktree, index) => TILE_TOP + (worktree.z - TILE_TOP) * lift(index, t));
  const states = edits.map((_, index) => edit(index, t));
  const privateCount = worktrees.map((_, index) =>
    states.filter((state, editIndex) => edits[editIndex].worktree === index && state.grow > 0.5).length);
  const lifted = worktrees.filter((_, index) => lift(index, t) > 0.5).length;
  const privateTotal = privateCount.reduce((sum, count) => sum + count, 0);

  const readout = focus === "base"
    ? `immutable base · ${GRID * GRID} blocks · read only`
    : typeof focus === "number"
      ? `${worktrees[focus].name} · ${privateCount[focus]} private · ${GRID * GRID - privateCount[focus]} shared`
      : `${lifted} worktree${lifted === 1 ? "" : "s"} · ${privateTotal} private block${privateTotal === 1 ? "" : "s"}`;

  // Fixed bands at each layer's resting height, so a layer that is still moving
  // cannot slide out from under the pointer.
  const bands = [corner(TILE_TOP)[1], ...worktrees.map((worktree) => corner(worktree.z)[1])];
  const pick = (event: PointerEvent<HTMLDivElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    const y = ((event.clientY - box.top) / box.height) * HEIGHT;
    let nearest = 0;
    bands.forEach((band, index) => {
      if (Math.abs(band - y) < Math.abs(bands[nearest] - y)) nearest = index;
    });
    if (Math.abs(bands[nearest] - y) > 40) return null;
    return nearest === 0 ? "base" : nearest - 1;
  };

  return (
    <div className="storage-figure">
      <div
        className="figure-stage"
        ref={stage}
        style={{ aspectRatio: `${WIDTH} / ${HEIGHT}` }}
        onPointerMove={(event) => { if (event.pointerType === "mouse") setFocus(pick(event)); }}
        onPointerLeave={(event) => { if (event.pointerType === "mouse") setFocus(null); }}
        onPointerDown={(event) => { if (event.pointerType !== "mouse") setFocus(pick(event)); }}
      >
        <svg viewBox={`0 0 ${WIDTH} ${HEIGHT}`} aria-hidden="true" focusable="false">
          <path className="fig-face fig-base-plate" d={basePlate.side} />
          <path className="fig-face fig-base-plate" d={basePlate.top} />
          {baseTiles.map((tile, index) => (
            <g className={`fig-base-tile${focus === "base" ? " is-lit" : ""}`} key={index}>
              <path className="fig-face fig-side" d={tile.side} />
              <path className="fig-face" d={tile.top} />
            </g>
          ))}

          {worktrees.map((worktree, index) => {
            const z = heights[index];
            const shown = Math.min(1, lift(index, t) * 4);
            const lit = focus === index || (focus === null && states.some((state, editIndex) =>
              state.fresh && state.grow < 1 && edits[editIndex].worktree === index));
            return (
              <g key={worktree.name} style={{ opacity: fixed(shown) }}>
                <path className={`fig-view${lit ? " is-lit" : ""}`} d={flat(view, plateOutline, z)} />
                {tiles.map((tile) => {
                  const [x, y] = view.at(tileStart(tile[0]) + TILE / 2, tileStart(tile[1]) + TILE / 2, z);
                  return <circle className="fig-shared" cx={fixed(x)} cy={fixed(y)} r="1.3" key={`${tile[0]}-${tile[1]}`} />;
                })}
                {edits.map((change, editIndex) => {
                  const height = states[editIndex].grow * BLOCK;
                  if (change.worktree !== index || height < 0.01) return null;
                  const block = solid(view, tileOutline(change.tile), z, z + height);
                  return (
                    <g className={`fig-private${states[editIndex].fresh ? " is-fresh" : ""}`} key={editIndex}>
                      <path className="fig-face fig-side" d={block.side} />
                      <path className="fig-face" d={block.top} />
                    </g>
                  );
                })}
              </g>
            );
          })}

          {edits.map((change, index) => {
            const { travel, grow, resetting } = states[index];
            // Copies only travel up; on reset the blocks shrink away in place.
            if (travel <= 0 || grow >= 1 || resetting) return null;
            const destination = heights[change.worktree];
            const z = TILE_TOP + (destination - TILE_TOP) * travel;
            const centerX = tileStart(change.tile[0]) + TILE / 2;
            const centerY = tileStart(change.tile[1]) + TILE / 2;
            const [x, from] = view.at(centerX, centerY, TILE_TOP);
            const [, to] = view.at(centerX, centerY, z);
            return (
              <g className="fig-copy" key={index} style={{ opacity: fixed(1 - grow) }}>
                <path className="fig-trail" d={`M${fixed(x)} ${fixed(from)}V${fixed(to)}`} />
                <path className="fig-ghost" d={flat(view, tileOutline(change.tile), z)} />
              </g>
            );
          })}

          <Callout from={corner(TILE_TOP)} progress={1} lit={focus === "base"} />
          {worktrees.map((worktree, index) => (
            <Callout from={corner(heights[index])} progress={wire(index, t)} lit={focus === index} key={worktree.name} />
          ))}
        </svg>

        <FigureLabel y={corner(TILE_TOP)[1]} shown lit={focus === "base"}>
          <span><b>00</b> immutable base</span>
          <small>a4d2c19 · read only</small>
        </FigureLabel>
        {worktrees.map((worktree, index) => (
          <FigureLabel y={corner(worktree.z)[1]} shown={wire(index, t) > 0.8} lit={focus === index} key={worktree.name}>
            <span>
              <b>{worktree.index}</b> {worktree.name}
              {privateCount[index] > 0 ? <em> +{privateCount[index]}</em> : null}
            </span>
            <small>{worktree.branch}</small>
          </FigureLabel>
        ))}
      </div>
      <div className="map-legend">
        <span><i className="legend-shared" aria-hidden="true" /> shared block</span>
        <span><i className="legend-private" aria-hidden="true" /> private edit</span>
        <span className="figure-readout" aria-hidden="true">{readout}</span>
      </div>
    </div>
  );
}

function Callout({ from: [x, y], progress, lit }: { from: Point; progress: number; lit: boolean }) {
  if (progress <= 0) return null;
  const end = LABEL_X - 8;
  return (
    <g className={`fig-callout${lit ? " is-lit" : ""}`}>
      <circle cx={fixed(x)} cy={fixed(y)} r="2.4" />
      <path d={`M${fixed(x)} ${fixed(y)}H${fixed(end)}`} pathLength={1} strokeDasharray="1 1" strokeDashoffset={fixed(1 - progress)} />
      {progress > 0.95 ? <path d={`M${fixed(end)} ${fixed(y - 4)}V${fixed(y + 4)}`} /> : null}
    </g>
  );
}

function FigureLabel({ y, shown, lit, children }: { y: number; shown: boolean; lit: boolean; children: React.ReactNode }) {
  return (
    <div
      className={`figure-label${shown ? " is-shown" : ""}${lit ? " is-lit" : ""}`}
      style={{ left: `${fixed((LABEL_X / WIDTH) * 100)}%`, top: `${fixed((y / HEIGHT) * 100)}%` }}
    >
      {children}
    </div>
  );
}
