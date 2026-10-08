import type { PointReference } from "./annotations.ts";

export type ScreenPoint = PointReference & {
  x: number;
  y: number;
  radius: number;
};

export function hitPoint(
  points: ScreenPoint[],
  pointer: { x: number; y: number },
) {
  let closest: ScreenPoint | null = null;
  let distance = Infinity;
  // Reverse order favors the topmost point when centers coincide.
  for (let index = points.length - 1; index >= 0; index--) {
    const point = points[index];
    const squared = (point.x - pointer.x) ** 2 + (point.y - pointer.y) ** 2;
    if (squared <= Math.max(8, point.radius + 3) ** 2 && squared < distance) {
      closest = point;
      distance = squared;
    }
  }
  return closest;
}

export function pointAction(
  active: boolean,
  pan: boolean,
  alt: boolean,
  hit: boolean,
) {
  if (pan || !active) return "pan";
  if (hit) return alt ? "delete" : "move";
  return alt ? "none" : "place";
}
