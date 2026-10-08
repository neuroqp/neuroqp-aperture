import {
  defaultPointSettings,
  pointColors,
  pointSizes,
  type PointSettings,
} from "./point-settings.ts";

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
  sourceIndex?: number;
};
export type AnnotationSettings = PointSettings & {
  labels: boolean;
  allSlices: boolean;
  counter: number;
};
export type ImageBounds = { width: number; height: number };

const imageJTypes = { Cross: 1, Dot: 2, Circle: 3 };
const imageJSizes = Object.keys(pointSizes) as PointSettings["size"][];
const imageJColor = (color: PointSettings["color"]) =>
  (0xff000000 | Number.parseInt(pointColors[color].slice(1), 16)) >>> 0;

export function settingsForCounter(
  rois: PointRoi[],
  counter: number,
): AnnotationSettings {
  const roi = rois.find((roi) =>
    roi.points.some((point) => point.counter === counter),
  );
  const point = roi?.points.find((point) => point.counter === counter);
  return {
    type:
      (Object.keys(imageJTypes) as PointSettings["type"][]).find(
        (type) => imageJTypes[type] === roi?.pointType,
      ) ?? defaultPointSettings.type,
    size: imageJSizes[roi?.size ?? -1] ?? defaultPointSettings.size,
    color:
      (Object.keys(pointColors) as PointSettings["color"][]).find(
        (color) => imageJColor(color) === roi?.color,
      ) ?? defaultPointSettings.color,
    labels: roi?.showLabels ?? true,
    allSlices:
      !!roi &&
      roi.position === 0 &&
      roi.channel === 0 &&
      roi.slice === 0 &&
      roi.frame === 0 &&
      point?.position === 0,
    counter,
  };
}

export function updateCounter<K extends keyof PointSettings | "labels">(
  rois: PointRoi[],
  counter: number,
  key: K,
  value: AnnotationSettings[K],
): PointRoi[] {
  const field = (
    {
      type: "pointType",
      size: "size",
      color: "color",
      labels: "showLabels",
    } as const
  )[key];
  const encoded =
    key === "type"
      ? imageJTypes[value as PointSettings["type"]]
      : key === "size"
        ? imageJSizes.indexOf(value as PointSettings["size"])
        : key === "color"
          ? imageJColor(value as PointSettings["color"])
          : value;
  let changed = false;
  const next = rois.flatMap((roi) => {
    const selected = roi.points.filter((point) => point.counter === counter);
    if (!selected.length || roi[field] === encoded) return [roi];
    changed = true;
    const remaining = roi.points.filter((point) => point.counter !== counter);
    const updated = { ...roi, [field]: encoded, points: selected };
    return remaining.length
      ? [{ ...roi, points: remaining }, updated]
      : [updated];
  });
  return changed ? next : rois;
}

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
  const existing = rois.find((roi) =>
    roi.points.some((point) => point.counter === settings.counter),
  );
  const style = {
    pointType: existing?.pointType ?? imageJTypes[settings.type],
    size: existing?.size ?? imageJSizes.indexOf(settings.size),
    color: existing ? existing.color : imageJColor(settings.color),
    showLabels: existing?.showLabels ?? settings.labels,
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
  const index = rois.findIndex(
    (roi) =>
      roi.points.some((point) => point.counter === settings.counter) &&
      roi.pointType === style.pointType &&
      roi.size === style.size &&
      roi.color === style.color &&
      roi.showLabels === style.showLabels &&
      roi.position <= 1 &&
      roi.channel <= 1 &&
      roi.slice <= 1 &&
      roi.frame <= 1 &&
      (!settings.allSlices ||
        (roi.position === 0 &&
          roi.channel === 0 &&
          roi.slice === 0 &&
          roi.frame === 0)),
  );
  if (index !== -1)
    return rois.map((roi, roiIndex) =>
      roiIndex === index ? { ...roi, points: [...roi.points, nextPoint] } : roi,
    );
  return [
    ...rois,
    { ...existing, ...style, points: [nextPoint], raw: existing?.raw ?? [] },
  ];
}

export function counterCount(rois: PointRoi[], counter: number) {
  return rois.reduce(
    (total, roi) =>
      total + roi.points.filter((point) => point.counter === counter).length,
    0,
  );
}
