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
  // ImageJ setCounter remembers the highest selected category during a session.
  nCounters?: number;
  overlay?: boolean;
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

export function settingsForRois(
  rois: PointRoi[],
  counter: number,
): AnnotationSettings {
  const roi = activeRoi(rois);
  const point = roi?.points.at(-1);
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

export function updatePointRois<K extends keyof PointSettings | "labels">(
  rois: PointRoi[],
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
  const next = rois.map((roi, index) => {
    if (
      (key !== "labels" &&
        (index !== rois.length - 1 || roi.overlay || !onFirstPage(roi))) ||
      roi[field] === encoded
    )
      return roi;
    changed = true;
    return { ...roi, [field]: encoded };
  });
  return changed ? next : rois;
}

export function onFirstPage(roi: PointRoi) {
  return (
    (roi.position <= 1 || roi.position === 0xfffffffe) &&
    roi.channel <= 1 &&
    roi.slice <= 1 &&
    roi.frame <= 1
  );
}

export function activeRoi(rois: PointRoi[]) {
  const roi = rois.at(-1);
  return roi?.overlay ? undefined : roi;
}

export function numberOfCounters(roi: PointRoi) {
  return roi.points.reduce(
    (count, point) => Math.max(count, point.counter + 1),
    roi.nCounters ?? 1,
  );
}

export function selectCounter(rois: PointRoi[], counter: number) {
  const roi = activeRoi(rois);
  if (!roi || !onFirstPage(roi) || counter < numberOfCounters(roi)) return rois;
  return [...rois.slice(0, -1), { ...roi, nCounters: counter + 1 }];
}

// java.awt.Color constants used by ImageJ PointRoi.getColor, categories 0-9.
const counterColors = [
  "#ffff00",
  "#ff00ff",
  "#00ffff",
  "#ffc800",
  "#00ff00",
  "#0000ff",
  "#ffffff",
  "#404040",
  "#ffafaf",
  "#c0c0c0",
];

export function pointAppearance(
  roi: PointRoi,
  index: number,
  multiple = numberOfCounters(roi) > 1,
) {
  const counter = roi.points[index].counter;
  if (multiple && !counterColors[counter]) {
    // ImageJ caches random colors for imported categories beyond its first ten.
    const color = Math.floor(Math.random() * 0x1000000);
    counterColors[counter] = `#${color.toString(16).padStart(6, "0")}`;
  }
  return {
    color: multiple
      ? counterColors[counter]
      : roi.color === null
        ? "#ffff00"
        : `#${(roi.color & 0xffffff).toString(16).padStart(6, "0")}`,
    label:
      roi.showLabels && roi.points.length > 1
        ? String(multiple ? counter : index + 1)
        : null,
  };
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
  const existing = activeRoi(rois);
  const nextPoint = {
    ...point,
    counter: settings.counter,
    position: settings.allSlices ? 0 : 1,
  };
  const visible = existing && onFirstPage(existing);
  const allSlicesScope =
    existing &&
    existing.position === 0 &&
    existing.channel === 0 &&
    existing.slice === 0 &&
    existing.frame === 0;
  if (visible && (!settings.allSlices || allSlicesScope)) {
    // Per-point positions can vary without changing the imported ROI's scope.
    return [
      ...rois.slice(0, -1),
      {
        ...existing,
        nCounters: Math.max(numberOfCounters(existing), settings.counter + 1),
        points: [...existing.points, nextPoint],
      },
    ];
  }
  return [
    ...rois,
    {
      ...(visible ? existing : {}),
      points: [nextPoint],
      pointType: visible ? existing.pointType : imageJTypes[settings.type],
      size: visible ? existing.size : imageJSizes.indexOf(settings.size),
      color: visible ? existing.color : imageJColor(settings.color),
      showLabels: visible ? existing.showLabels : settings.labels,
      position: nextPoint.position,
      channel: 0,
      slice: 0,
      frame: 0,
      nCounters: settings.counter + 1,
      raw: visible ? existing.raw : [],
    },
  ];
}

export function counterCount(rois: PointRoi[], counter: number) {
  return (
    activeRoi(rois)?.points.filter((point) => point.counter === counter)
      .length ?? 0
  );
}
