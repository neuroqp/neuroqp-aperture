import { pointColors, type PointSettings } from "./point-settings.ts";

export type AnnotationPoint = {
  x: number;
  y: number;
  counter: number;
  position: number;
};
export type PointRoi = {
  points: AnnotationPoint[];
  pointType: number;
  size: number;
  color: number | null;
  showLabels: boolean;
  position: number;
  channel: number;
  slice: number;
  frame: number;
  raw: number[];
};
export type AnnotationSettings = PointSettings & {
  labels: boolean;
  allSlices: boolean;
  counter: number;
};
export type ImageBounds = { width: number; height: number };

// OSD handles rotation in its coordinate conversions, but not image flipping.
export function flipCoordinate(
  point: { x: number; y: number },
  width: number,
  flipped: boolean,
) {
  return { x: flipped ? width - point.x : point.x, y: point.y };
}

// ImageJ coordinates place (0,0) at the center of the first stored pixel.
export function imageToStored(
  point: { x: number; y: number },
  width: number,
  flipped: boolean,
) {
  const stored = flipCoordinate(point, width, flipped);
  return { x: stored.x - 0.5, y: stored.y - 0.5 };
}

export function storedToImage(
  point: { x: number; y: number },
  width: number,
  flipped: boolean,
) {
  return flipCoordinate({ x: point.x + 0.5, y: point.y + 0.5 }, width, flipped);
}

export function addPoint(
  rois: PointRoi[],
  point: { x: number; y: number },
  settings: AnnotationSettings,
  bounds: ImageBounds,
): PointRoi[] | null {
  if (
    !Number.isFinite(point.x) ||
    !Number.isFinite(point.y) ||
    point.x < -0.5 ||
    point.y < -0.5 ||
    point.x >= bounds.width - 0.5 ||
    point.y >= bounds.height - 0.5 ||
    !Number.isInteger(settings.counter) ||
    settings.counter < 0 ||
    settings.counter > 9
  )
    return null;
  const style = {
    pointType: { Cross: 1, Dot: 2, Circle: 3 }[settings.type],
    size: ["Tiny", "Small", "Medium", "Large", "Extra large"].indexOf(
      settings.size,
    ),
    color:
      (0xff000000 |
        Number.parseInt(pointColors[settings.color].slice(1), 16)) >>>
      0,
    showLabels: settings.labels,
    position: settings.allSlices ? 0 : 1,
    channel: 0,
    slice: 0,
    frame: 0,
  };
  const nextPoint = {
    ...point,
    counter: settings.counter,
    position: style.position,
  };
  const last = rois.at(-1);
  if (
    last &&
    (Object.keys(style) as (keyof typeof style)[]).every(
      (key) => last[key] === style[key],
    )
  ) {
    return [
      ...rois.slice(0, -1),
      { ...last, points: [...last.points, nextPoint] },
    ];
  }
  return [...rois, { ...style, points: [nextPoint], raw: [] }];
}

export function counterCount(rois: PointRoi[], counter: number) {
  return rois.reduce(
    (total, roi) =>
      total + roi.points.filter((point) => point.counter === counter).length,
    0,
  );
}
