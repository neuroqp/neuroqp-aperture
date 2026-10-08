import assert from "node:assert/strict";
import { test } from "node:test";
import {
  addPoint,
  movePoint,
  deletePoint,
  counterCount,
  flipCoordinate,
  imageToStored,
  settingsForRois,
  storedToImage,
  updatePointRois,
  pointAppearance,
  selectCounter,
  type AnnotationSettings,
  type PointRoi,
} from "./annotations.ts";

const settings: AnnotationSettings = {
  type: "Cross",
  color: "Purple",
  size: "Medium",
  labels: true,
  allSlices: false,
  counter: 2,
};
const bounds = { width: 4000, height: 2000 };

const mixed: PointRoi & { name: string } = {
  points: [
    { x: 12.25, y: 33.75, counter: 2, position: 1 },
    { x: 50, y: 60, counter: 3, position: 8 },
    { x: 91, y: 100, counter: 2, position: 9 },
  ],
  pointType: 0,
  size: 6,
  color: 0x80123456,
  showLabels: false,
  position: 0,
  channel: 0,
  slice: 0,
  frame: 0,
  raw: [73, 111, 117, 116],
  sourceIndex: 7,
  name: "Imported group",
};

test("counters append categories to one ROI and share its style", () => {
  const first = addPoint([], { x: 100.25, y: 900.5 }, settings, bounds)!;
  const second = addPoint(
    first,
    { x: 111, y: 222 },
    { ...settings, counter: 3 },
    bounds,
  )!;
  assert.equal(first[0].points.length, 1);
  assert.equal(second.length, 1);
  assert.deepEqual(
    second[0].points.map((point) => point.counter),
    [2, 3],
  );
  const changed = updatePointRois(second, "color", "Cyan");
  const third = addPoint(changed, { x: 1, y: 1 }, settings, bounds)!;
  assert.equal(third.length, 1);
  assert.equal(third[0].color, 0xff00bbce);
  assert.equal(third[0].points.length, 3);
  assert.deepEqual(settingsForRois(third, 3), {
    ...settings,
    color: "Cyan",
    counter: 3,
  });
  assert.equal(counterCount(third, 2), 2);
  assert.equal(counterCount(third, 3), 1);
  assert.equal(third[0].points[0].position, 1);
});

test("ROI-wide edits preserve point order and metadata without splitting or changing other ROIs", () => {
  const overlay = { ...mixed, sourceIndex: 0 };
  const before = structuredClone(mixed);
  const rois = [overlay, mixed];
  const color = updatePointRois(rois, "color", "Cyan");
  const type = updatePointRois(color, "type", "Circle");
  const updated = updatePointRois(type, "size", "Small");
  assert.equal(updated.length, 2);
  assert.strictEqual(updated[0], overlay);
  assert.deepEqual(updated[1], {
    ...mixed,
    color: 0xff00bbce,
    pointType: 3,
    size: 1,
  });
  assert.strictEqual(updated[1].points, mixed.points);
  assert.strictEqual(updated[1].raw, mixed.raw);
  assert.deepEqual(mixed, before);
  assert.strictEqual(updatePointRois(updated, "size", "Small"), updated);
  assert.equal(counterCount(updated, 2), 2);
});

test("labels apply to active ROI and all overlays without resetting imported styles", () => {
  const hidden = { ...mixed, position: 5, channel: 2, slice: 5, frame: 3 };
  const updated = updatePointRois([mixed, hidden], "labels", true);
  assert.deepEqual(updated, [
    { ...mixed, showLabels: true },
    { ...hidden, showLabels: true },
  ]);
  assert.equal(mixed.showLabels, false);
  assert.equal(hidden.showLabels, false);
  assert.deepEqual(settingsForRois([], 9), { ...settings, counter: 9 });
});

test("appending preserves imported point positions and raw styles across categories", () => {
  const next = addPoint(
    [mixed],
    { x: 3, y: 4 },
    { ...settings, counter: 0 },
    bounds,
  )!;
  assert.equal(next.length, 1);
  assert.deepEqual(next[0].points.slice(0, 3), mixed.points);
  assert.deepEqual(next[0].points[3], { x: 3, y: 4, counter: 0, position: 1 });
  assert.equal(next[0].pointType, 0);
  assert.equal(next[0].size, 6);
  assert.equal(next[0].color, 0x80123456);
  assert.equal(next[0].sourceIndex, 7);
  assert.equal(next[0].position, mixed.position);
  assert.equal((next[0] as typeof mixed).name, "Imported group");
  const allSlices = addPoint(
    next,
    { x: 5, y: 6 },
    { ...settings, allSlices: true },
    bounds,
  )!;
  assert.equal(allSlices.length, 1);
  assert.equal(allSlices[0].points[4].position, 0);
});

test("all-slice additions preserve an existing fixed-slice ROI and use a new compatible scope", () => {
  const fixed = { ...mixed, position: 1, slice: 1 };
  const next = addPoint(
    [fixed],
    { x: 5, y: 6 },
    { ...settings, allSlices: true },
    bounds,
  )!;
  assert.equal(next.length, 2);
  assert.deepEqual(next[0], { ...fixed, overlay: true });
  assert.equal(next[1].position, 0);
  assert.equal(next[1].slice, 0);
  assert.equal(next[1].points[0].position, 0);
  assert.equal(next[1].pointType, fixed.pointType);
  assert.equal(next[1].size, fixed.size);
  assert.equal(next[1].sourceIndex, fixed.sourceIndex);
});

test("hidden active ROIs stay untouched and new points get a fresh first-page ROI", () => {
  const hidden = { ...mixed, position: 4, channel: 2, slice: 4, frame: 3 };
  assert.strictEqual(updatePointRois([hidden], "size", "Small")[0], hidden);
  assert.strictEqual(selectCounter([hidden], 9)[0], hidden);
  const next = addPoint([mixed, hidden], { x: 3, y: 4 }, settings, bounds)!;
  assert.strictEqual(next[0], mixed);
  assert.deepEqual(next[1], { ...hidden, overlay: true });
  assert.equal(next.length, 3);
  assert.equal(next[2].position, 1);
  assert.equal(next[2].channel, 0);
  assert.equal(next[2].slice, 0);
  assert.equal(next[2].frame, 0);
  assert.equal(next[2].sourceIndex, undefined);
  assert.deepEqual(next[2].raw, []);
});

test("default counter0 labels use point indices within each ROI and hide single-point labels", () => {
  const first = addPoint(
    [],
    { x: 1, y: 2 },
    { ...settings, counter: 0 },
    bounds,
  )!;
  assert.deepEqual(pointAppearance(first[0], 0), {
    color: "#6c3e89",
    label: null,
  });
  const second = addPoint(
    first,
    { x: 3, y: 4 },
    { ...settings, counter: 0 },
    bounds,
  )!;
  assert.deepEqual(pointAppearance(second[0], 0), {
    color: "#6c3e89",
    label: "1",
  });
  assert.equal(pointAppearance(second[0], 1).label, "2");
  assert.equal(
    pointAppearance({ ...second[0], showLabels: false }, 1).label,
    null,
  );
});

test("selecting a higher counter latches category colors and labels when returning to 0", () => {
  const first = addPoint(
    [],
    { x: 1, y: 2 },
    { ...settings, counter: 0 },
    bounds,
  )!;
  const second = addPoint(
    first,
    { x: 3, y: 4 },
    { ...settings, counter: 0 },
    bounds,
  )!;
  const selected = selectCounter(second, 1);
  assert.equal(selected[0].nCounters, 2);
  assert.deepEqual(pointAppearance(selected[0], 0), {
    color: "#ffff00",
    label: "0",
  });
  assert.strictEqual(selectCounter(selected, 0), selected);
  const mixed = addPoint(
    selected,
    { x: 5, y: 6 },
    { ...settings, counter: 1 },
    bounds,
  )!;
  assert.deepEqual(pointAppearance(mixed[0], 2), {
    color: "#ff00ff",
    label: "1",
  });
  assert.equal(
    pointAppearance(
      addPoint(mixed, { x: 7, y: 8 }, { ...settings, counter: 0 }, bounds)![0],
      3,
    ).label,
    "0",
  );
});

test("a nonzero category alone still uses ImageJ category labels and exact palette", () => {
  const points = Array.from({ length: 10 }, (_, counter) => ({
    x: counter,
    y: 0,
    counter,
    position: 0,
  }));
  const roi = { ...mixed, points, showLabels: true };
  assert.deepEqual(
    points.map((_, index) => pointAppearance(roi, index)),
    [
      { color: "#ffff00", label: "0" },
      { color: "#ff00ff", label: "1" },
      { color: "#00ffff", label: "2" },
      { color: "#ffc800", label: "3" },
      { color: "#00ff00", label: "4" },
      { color: "#0000ff", label: "5" },
      { color: "#ffffff", label: "6" },
      { color: "#404040", label: "7" },
      { color: "#ffafaf", label: "8" },
      { color: "#c0c0c0", label: "9" },
    ],
  );
  const onlyOne = { ...roi, points: [points[1], { ...points[1], x: 2 }] };
  assert.deepEqual(pointAppearance(onlyOne, 0), {
    color: "#ff00ff",
    label: "1",
  });
  assert.deepEqual(pointAppearance(onlyOne, 1), {
    color: "#ff00ff",
    label: "1",
  });
});

test("imported categories above 9 keep a stable randomly assigned ImageJ-style category color", () => {
  const roi = {
    ...mixed,
    points: [
      { ...mixed.points[0], counter: 11 },
      { ...mixed.points[1], counter: 11 },
    ],
    showLabels: true,
  };
  const first = pointAppearance(roi, 0);
  assert.match(first.color, /^#[0-9a-f]{6}$/);
  assert.equal(first.label, "11");
  assert.deepEqual(pointAppearance(roi, 1), first);
});

test("overlay-only imports keep their styles and get a separate active selection", () => {
  const overlay = { ...mixed, overlay: true };
  const rois = [overlay];
  assert.strictEqual(updatePointRois(rois, "size", "Small"), rois);
  assert.strictEqual(selectCounter(rois, 9), rois);
  assert.equal(counterCount(rois, 2), 0);
  assert.deepEqual(settingsForRois(rois, 2), settings);
  const next = addPoint(rois, { x: 1, y: 2 }, settings, bounds)!;
  assert.equal(next.length, 2);
  assert.strictEqual(next[0], overlay);
  assert.ok(!next[1].overlay);
  assert.equal(next[1].pointType, 1);
  assert.equal(counterCount(next, 2), 1);
});

test("ImageJ mixed-position sentinel displays and preserves per-point stack positions", () => {
  const roi = { ...mixed, position: 0xfffffffe };
  const next = addPoint([roi], { x: 1, y: 2 }, settings, bounds)!;
  assert.equal(next.length, 1);
  assert.equal(next[0].position, roi.position);
  assert.deepEqual(next[0].points.slice(0, 3), mixed.points);
  assert.equal(next[0].points[3].position, 1);
  assert.equal(updatePointRois(next, "size", "Small")[0].size, 1);
});

test("outside clicks and invalid counters cannot add points", () => {
  for (const point of [
    { x: -1, y: 0 },
    { x: 4000, y: 0 },
    { x: 0, y: 2000 },
    { x: NaN, y: 1 },
    { x: 1, y: Infinity },
  ]) {
    assert.equal(addPoint([], point, settings, bounds), null);
  }
  assert.equal(
    addPoint([], { x: 1, y: 1 }, { ...settings, counter: 100 }, bounds),
    null,
  );
});

test("flip conversion is its own inverse and preserves subpixel stored coordinates", () => {
  const point = { x: 100.25, y: 555.75 };
  assert.deepEqual(flipCoordinate(point, bounds.width, false), point);
  assert.deepEqual(flipCoordinate(point, bounds.width, true), {
    x: 3899.75,
    y: 555.75,
  });
  assert.deepEqual(
    flipCoordinate(
      flipCoordinate(point, bounds.width, true),
      bounds.width,
      true,
    ),
    point,
  );
});

test("stored pixel centers align with OSD pixel areas, including mirrored images", () => {
  for (const flipped of [false, true]) {
    for (const stored of [
      { x: 0, y: 0 },
      { x: 3999, y: 1999 },
      { x: 123.25, y: 456.75 },
    ]) {
      const image = storedToImage(stored, bounds.width, flipped);
      assert.deepEqual(imageToStored(image, bounds.width, flipped), stored);
    }
  }
  assert.deepEqual(storedToImage({ x: 0, y: 0 }, 4000, false), {
    x: 0.5,
    y: 0.5,
  });
  assert.deepEqual(storedToImage({ x: 0, y: 0 }, 4000, true), {
    x: 3999.5,
    y: 0.5,
  });
  assert.ok(addPoint([], { x: -0.5, y: -0.5 }, settings, bounds));
  assert.equal(addPoint([], { x: 3999.5, y: 0 }, settings, bounds), null);
});

test("moving a visible overlay point preserves categories, positions, styles and metadata", () => {
  const overlay = { ...mixed, overlay: true };
  const rois = [overlay, { ...mixed, sourceIndex: 8 }];
  const reference = { roiIndex: 0, pointIndex: 0 };
  const before = structuredClone(rois);
  const moved = movePoint(rois, reference, { x: -0.25, y: 12.125 }, bounds);
  assert.deepEqual(moved[0], {
    ...overlay,
    points: [
      { ...mixed.points[0], x: -0.25, y: 12.125 },
      ...mixed.points.slice(1),
    ],
  });
  assert.strictEqual(moved[1], rois[1]);
  assert.strictEqual(moved[0].raw, overlay.raw);
  assert.deepEqual(rois, before);
  assert.strictEqual(
    movePoint(moved, reference, { x: -0.25, y: 12.125 }, bounds),
    moved,
  );
  assert.strictEqual(
    movePoint(rois, { roiIndex: 8, pointIndex: 0 }, { x: 2, y: 3 }, bounds),
    rois,
  );
  assert.strictEqual(
    movePoint(rois, reference, { x: NaN, y: 3 }, bounds),
    rois,
  );
  const edge = movePoint(rois, reference, { x: -100, y: 5000 }, bounds)[0]
    .points[0];
  assert.equal(edge.x, -0.5);
  assert.ok(edge.y < bounds.height - 0.5);
  assert.ok(edge.y > bounds.height - 0.500001);
});

test("deletion keeps empty active selection metadata and never promotes a previous ROI", () => {
  const previous = { ...mixed, sourceIndex: 0 };
  const active = { ...mixed, points: [mixed.points[0]], sourceIndex: 7 };
  const rois = [previous, active];
  const deleted = deletePoint(rois, { roiIndex: 1, pointIndex: 0 });
  assert.strictEqual(deleted[0], previous);
  assert.deepEqual(deleted[1], { ...active, points: [], nCounters: 3 });
  assert.equal(counterCount(deleted, 2), 0);
  assert.equal(settingsForRois(deleted, 0).labels, active.showLabels);
  assert.equal(active.points.length, 1);
  assert.strictEqual(
    deletePoint(deleted, { roiIndex: 1, pointIndex: 0 }),
    deleted,
  );
  const next = addPoint(deleted, { x: 2, y: 3 }, settings, bounds)!;
  assert.equal(next.length, 2);
  assert.equal(next[1].sourceIndex, 7);
  assert.equal(next[1].points.length, 1);
});

test("the ImageJ counter range includes 99 and rejects 100", () => {
  assert.equal(
    addPoint([], { x: 1, y: 2 }, { ...settings, counter: 99 }, bounds)![0]
      .points[0].counter,
    99,
  );
  assert.equal(
    addPoint([], { x: 1, y: 2 }, { ...settings, counter: 100 }, bounds),
    null,
  );
});

test("a new selection demotes the previous active ROI even when the new selection is later emptied", () => {
  const original = { ...mixed, position: 1, slice: 1, overlay: false };
  const added = addPoint(
    [original],
    { x: 5, y: 6 },
    { ...settings, allSlices: true },
    bounds,
  )!;
  assert.deepEqual(added[0], { ...original, overlay: true });
  assert.strictEqual(added[0].raw, original.raw);
  const deleted = deletePoint(added, { roiIndex: 1, pointIndex: 0 });
  assert.equal(deleted[0].overlay, true);
  assert.equal(deleted[1].points.length, 0);
  assert.equal(counterCount(deleted, 2), 0);
  assert.equal(original.overlay, false);
});
