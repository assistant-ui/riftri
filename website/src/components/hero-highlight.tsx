"use client";

import { useMemo, useRef, useState, type CSSProperties } from "react";
import { clamp01, easeOut, smooth, span, usePlayhead } from "./figure-kit";

// One loop, in seconds, drawn like the farmjs.dev hero selection: a cursor
// glides in, drags a dashed selection open over the word, the selection fills
// with the accent a pixel at a time, lets go, holds, then empties.
const STEPS = [
  ["rest", 1.2],
  ["glide", 0.6],
  ["press", 0.1],
  ["drag", 0.75],
  ["release", 0.12],
  ["fill", 0.9],
  ["deselect", 0.45],
  ["hold", 3.6],
  ["empty", 0.7],
] as const;
type Step = (typeof STEPS)[number][0];
const AT = {} as Record<Step, number>;
const LENGTH = {} as Record<Step, number>;
let elapsed = 0;
for (const [step, length] of STEPS) {
  AT[step] = elapsed;
  LENGTH[step] = length;
  elapsed += length;
}
const PERIOD = elapsed;
// Server render, first paint, and reduced motion show the finished highlight.
const REST = AT.hold + 0.3;
const progress = (t: number, step: Step) => span(t, AT[step], LENGTH[step]);
const lerp = (from: number, to: number, amount: number) => from + (to - from) * amount;

const COLUMNS = 28;
const ROWS = 9;
// A fixed hash keeps every cell's turn identical on the server and the client.
const noise = (column: number, row: number) => {
  const value = Math.sin(column * 12.9898 + row * 78.233) * 43758.5453;
  return value - Math.floor(value);
};
const cells = Array.from({ length: COLUMNS * ROWS }, (_, index) => {
  const column = index % COLUMNS;
  const row = Math.floor(index / COLUMNS);
  // Mostly left to right, like a loading bar, with each cell a little early or late.
  return { column, row, at: ((column / COLUMNS) * 0.6 + noise(column, row) * 0.4).toFixed(3) };
});

export function HeroHighlight({ children }: { children: string }) {
  const mark = useRef<HTMLElement>(null);
  const [held, setHeld] = useState(false);
  const t = usePlayhead(mark, { period: PERIOD, rest: REST, hold: held });

  const drag = smooth(progress(t, "drag"));
  const deselect = smooth(progress(t, "deselect"));
  const fill = clamp01(easeOut(progress(t, "fill")) - smooth(progress(t, "empty")));
  const filled = t >= AT.deselect && t < AT.empty;

  // The selection opens from the top-left corner to the bottom-right one, then
  // collapses into the bottom-right corner as it lets go.
  const selection = t >= AT.drag && t < AT.hold
    ? { from: t < AT.deselect ? 0 : deselect, to: t < AT.release ? drag : 1, opacity: 1 - deselect }
    : null;

  // The cursor glides in from above left to the top-left corner, drags to the
  // bottom-right corner, then slips away as the fill starts. Offsets are pixels.
  let cursor: { at: number; dx: number; dy: number; opacity: number; pressed: boolean } | null = null;
  if (t >= AT.glide && t < AT.press) {
    const glide = smooth(progress(t, "glide"));
    cursor = { at: 0, dx: lerp(-34, 0, glide), dy: lerp(-26, 0, glide), opacity: glide, pressed: false };
  } else if (t >= AT.press && t < AT.release) {
    cursor = { at: t < AT.drag ? 0 : drag, dx: 0, dy: 0, opacity: 1, pressed: true };
  } else if (t >= AT.release && t < AT.fill + 0.4) {
    const away = smooth(span(t, AT.release + 0.1, 0.42));
    cursor = { at: 1, dx: 10 * away, dy: 10 * away, opacity: 1 - away, pressed: false };
  }

  const pixels = useMemo(
    () => cells.map(({ column, row, at }) => (
      <rect x={column} y={row} width="1.02" height="1.02" style={{ "--at": at } as CSSProperties} key={`${column}-${row}`} />
    )),
    [],
  );

  const className = [
    "hero-highlight",
    filled ? "" : "is-plain",
    // The text darkens once most of the word is orange behind it.
    filled || fill > 0.8 ? "" : "is-unlit",
  ].filter(Boolean).join(" ");

  return (
    <mark
      ref={mark}
      className={className}
      onPointerEnter={(event) => { if (event.pointerType === "mouse") setHeld(true); }}
      onPointerLeave={(event) => { if (event.pointerType === "mouse") setHeld(false); }}
    >
      {!filled ? (
        <svg
          className="highlight-pixels"
          viewBox={`0 0 ${COLUMNS} ${ROWS}`}
          preserveAspectRatio="none"
          shapeRendering="crispEdges"
          aria-hidden="true"
          focusable="false"
          style={{ "--fill": (fill * 1.05).toFixed(3) } as CSSProperties}
        >
          {pixels}
        </svg>
      ) : null}
      <span className="highlight-text">{children}</span>
      {selection ? (
        <span
          className={`highlight-selection${selection.to - selection.from > 0.08 ? " has-corners" : ""}`}
          aria-hidden="true"
          style={{
            "--from": selection.from.toFixed(4),
            "--to": selection.to.toFixed(4),
            opacity: selection.opacity.toFixed(3),
          } as CSSProperties}
        >
          <i className="corner-tl" /><i className="corner-tr" /><i className="corner-bl" /><i className="corner-br" />
        </span>
      ) : null}
      {cursor ? (
        <svg
          className="highlight-cursor"
          viewBox="0 0 24 24"
          aria-hidden="true"
          focusable="false"
          style={{
            "--at": cursor.at.toFixed(4),
            "--dx": `${cursor.dx.toFixed(2)}px`,
            "--dy": `${cursor.dy.toFixed(2)}px`,
            opacity: cursor.opacity.toFixed(3),
            transform: `scale(${cursor.pressed ? 0.86 : 1})`,
          } as CSSProperties}
        >
          <path d="M4 2.5 19.5 10l-6.6 1.9L9.7 18.6z" />
        </svg>
      ) : null}
    </mark>
  );
}
