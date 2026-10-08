import { useEffect, useState, type RefObject } from "react";

export type Point = [number, number];

// Server and browser can disagree in the last bit of a sine, which would flip a
// printed coordinate and break hydration, so trigonometry is snapped and every
// coordinate is printed with two decimals.
const snap = (value: number) => Math.round(value * 1e9) / 1e9;
export const fixed = (value: number) => (Math.abs(value) < 0.005 ? 0 : value).toFixed(2);

export const clamp01 = (value: number) => Math.min(1, Math.max(0, value));
/** Progress through a window that starts at `start` and lasts `length`. */
export const span = (t: number, start: number, length: number) => clamp01((t - start) / length);
export const smooth = (value: number) => value * value * (3 - 2 * value);
export const easeOut = (value: number) => 1 - (1 - value) ** 3;

export type Lens = {
  at: (x: number, y: number, z: number) => Point;
  /** Larger is nearer the viewer, for painter's order. */
  depth: (x: number, y: number) => number;
};

/** World units to screen: turn by `yaw` about the vertical axis, tilt by `pitch`, then scale and shift. */
export function lens(yaw: number, pitch: number, scale: number, shiftX: number, shiftY: number): Lens {
  const [cy, sy, cp, sp] = [Math.cos(yaw), Math.sin(yaw), Math.cos(pitch), Math.sin(pitch)].map(snap);
  return {
    at: (x, y, z) => [(x * cy - y * sy) * scale + shiftX, ((x * sy + y * cy) * sp - z * cp) * scale + shiftY],
    depth: (x, y) => x * sy + y * cy,
  };
}

/** A rounded rectangle on a horizontal plane, as world points. */
export function roundedRect(x0: number, y0: number, x1: number, y1: number, radius: number): Point[] {
  const corners: [number, number, number][] = [
    [x1 - radius, y0 + radius, -90],
    [x1 - radius, y1 - radius, 0],
    [x0 + radius, y1 - radius, 90],
    [x0 + radius, y0 + radius, 180],
  ];
  const points: Point[] = [];
  for (const [cx, cy, start] of corners) {
    for (let step = 0; step <= 5; step++) {
      const angle = ((start + step * 18) * Math.PI) / 180;
      points.push([cx + radius * Math.cos(angle), cy + radius * Math.sin(angle)]);
    }
  }
  return points;
}

export const polyline = (points: Point[]) => `M${points.map(([x, y]) => `${fixed(x)} ${fixed(y)}`).join("L")}`;
export const polygon = (points: Point[]) => `${polyline(points)}Z`;

/** The outline of a flat shape lying at height `z`. */
export function flat(view: Lens, outline: Point[], z: number): string {
  return polygon(outline.map(([x, y]) => view.at(x, y, z)));
}

/**
 * A rounded solid from `z0` up to `z1`: the top face and the side band that faces
 * the viewer. Faces are filled with the background, so paint order alone hides
 * whatever sits behind the solid.
 */
export function solid(view: Lens, outline: Point[], z0: number, z1: number): { top: string; side: string } {
  const top = outline.map(([x, y]) => view.at(x, y, z1));
  const bottom = outline.map(([x, y]) => view.at(x, y, z0));
  let left = 0;
  let right = 0;
  top.forEach(([x], index) => {
    if (x < top[left][0]) left = index;
    if (x > top[right][0]) right = index;
  });
  const walk = (direction: 1 | -1) => {
    const indices = [left];
    for (let index = left; index !== right; ) {
      index = (index + direction + top.length) % top.length;
      indices.push(index);
    }
    return indices;
  };
  const meanY = (indices: number[]) => indices.reduce((sum, index) => sum + top[index][1], 0) / indices.length;
  const forward = walk(1);
  const backward = walk(-1);
  const near = meanY(forward) > meanY(backward) ? forward : backward;
  return {
    top: polygon(top),
    side: polygon([...near.map((index) => top[index]), ...near.reverse().map((index) => bottom[index])]),
  };
}

/**
 * A clock in seconds that only runs while `target` is on screen and the reader
 * allows motion. It starts at `start` (the server-rendered pose, `rest` unless
 * given). Under reduced motion it stays at `rest`, a finished pose. `hold`
 * eases time to a stop instead of freezing it mid-step.
 */
export function usePlayhead(
  target: RefObject<Element | null>,
  { period, rest, start = rest, hold }: { period: number; rest: number; start?: number; hold: boolean },
): number {
  const [time, setTime] = useState(start);
  const [state] = useState(() => ({ time: start, speed: 1, hold }));
  state.hold = hold;

  useEffect(() => {
    const node = target.current;
    if (!node) return;
    const motion = matchMedia("(prefers-reduced-motion: reduce)");
    let frame = 0;
    let last = 0;
    let visible = false;

    const tick = () => {
      const now = performance.now();
      const step = last ? Math.min(1 / 30, (now - last) / 1000) : 0;
      last = now;
      state.speed += ((state.hold ? 0 : 1) - state.speed) * Math.min(1, step * 6);
      state.time = (state.time + step * state.speed) % period;
      setTime(state.time);
      frame = requestAnimationFrame(tick);
    };
    const sync = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      last = 0;
      if (motion.matches) {
        state.time = rest;
        setTime(rest);
      } else if (visible) {
        frame = requestAnimationFrame(tick);
      }
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
  }, [target, period, rest, state]);

  return time;
}
