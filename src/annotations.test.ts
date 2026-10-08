import assert from "node:assert/strict";
import { test } from "node:test";
import {
  addPoint,
  counterCount,
  flipCoordinate,
  imageToStored,
  settingsForCounter,
  storedToImage,
  updateCounter,
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

test("switching counters recalls their style and adds to their existing group", () => {
  const first = addPoint([], { x: 100.25, y: 900.5 }, settings, bounds)!;
  first[0].raw = [73, 111, 117, 116];
  const second = addPoint(
    first,
    { x: 111, y: 222 },
    { ...settings, counter: 3 },
    bounds,
  )!;
  assert.equal(first[0].points.length, 1);
  assert.equal(second.length, 2);
  assert.deepEqual(second[0].raw, first[0].raw);
  const changed = updateCounter(second, 2, "color", "Cyan");
  const third = addPoint(
    changed,
    { x: 1, y: 1 },
    { ...settings, color: "Cyan" },
    bounds,
  )!;
  assert.equal(third.length, 2);
  assert.equal(third[0].color, 0xff00bbce);
  assert.equal(third[1].color, 0xff6c3e89);
  assert.equal(third[0].points.length, 2);
  assert.strictEqual(third[1], second[1]);
  assert.deepEqual(settingsForCounter(third, 2), {
    ...settings,
    color: "Cyan",
  });
  assert.deepEqual(settingsForCounter(third, 3), { ...settings, counter: 3 });
  assert.equal(counterCount(third, 2), 2);
  assert.equal(counterCount(third, 3), 1);
  assert.equal(third[1].points[0].position, 1);
  assert.equal(
    addPoint(
      third,
      { x: 1, y: 1 },
      { ...settings, allSlices: true },
      bounds,
    )!.at(-1)!.position,
    0,
  );
});

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

test("editing a mixed ROI splits only the selected counter and preserves metadata", () => {
  const before = structuredClone(mixed);
  const rois = [mixed];
  const updated = updateCounter(rois, 2, "color", "Cyan");
  assert.equal(updated.length, 2);
  assert.deepEqual(updated[0], {
    ...mixed,
    points: [mixed.points[1]],
  });
  assert.deepEqual(updated[1], {
    ...mixed,
    color: 0xff00bbce,
    points: [mixed.points[0], mixed.points[2]],
  });
  assert.deepEqual(mixed, before);
  assert.strictEqual(updated[0].raw, mixed.raw);
  assert.strictEqual(updated[1].raw, mixed.raw);
  assert.strictEqual(updateCounter(updated, 2, "color", "Cyan"), updated);
  assert.strictEqual(updateCounter(rois, 9, "labels", true), rois);
});

test("labels update every ROI of a counter without resetting unsupported imported styles", () => {
  const hidden = {
    ...mixed,
    position: 5,
    channel: 2,
    slice: 5,
    frame: 3,
    points: [{ ...mixed.points[2], position: 5 }],
  };
  const rois = [mixed, hidden];
  const updated = updateCounter(rois, 2, "labels", true);
  assert.equal(updated.length, 3);
  assert.equal(updated[0].showLabels, false);
  assert.equal(updated[1].showLabels, true);
  assert.deepEqual(updated[2], { ...hidden, showLabels: true });
  for (const roi of updated) {
    assert.equal(roi.pointType, 0);
    assert.equal(roi.size, 6);
    assert.equal(roi.color, 0x80123456);
  }
  assert.equal(hidden.showLabels, false);
  assert.deepEqual(settingsForCounter(updated, 2), {
    ...settings,
    labels: true,
  });
  assert.deepEqual(settingsForCounter([], 9), { ...settings, counter: 9 });
});

test("type and size edits apply only their selected field to every counter point", () => {
  const type = updateCounter([mixed], 2, "type", "Circle");
  const sized = updateCounter(type, 2, "size", "Small");
  assert.deepEqual(sized[0], { ...mixed, points: [mixed.points[1]] });
  assert.deepEqual(sized[1], {
    ...mixed,
    pointType: 3,
    size: 1,
    points: [mixed.points[0], mixed.points[2]],
  });
  assert.deepEqual(settingsForCounter(sized, 2), {
    ...settings,
    type: "Circle",
    size: "Small",
    labels: false,
  });
});

test("new points inherit existing raw style while existing slice positions stay intact", () => {
  const updated = updateCounter([mixed], 2, "labels", true);
  const next = addPoint(updated, { x: 3, y: 4 }, settings, bounds)!;
  assert.equal(next.length, 2);
  assert.equal(next[1].points.length, 3);
  assert.deepEqual(next[1].points.slice(0, 2), [
    mixed.points[0],
    mixed.points[2],
  ]);
  assert.deepEqual(next[1].points[2], {
    x: 3,
    y: 4,
    counter: 2,
    position: 1,
  });
  assert.equal(next[1].pointType, 0);
  assert.equal(next[1].size, 6);
  assert.equal(next[1].color, 0x80123456);
  assert.equal(next[1].sourceIndex, 7);
  assert.strictEqual(next[0], updated[0]);
});

test("adding on the current slice never reassigns imported ROI stack positions", () => {
  const hidden = {
    ...mixed,
    position: 4,
    channel: 2,
    slice: 4,
    frame: 3,
  };
  const next = addPoint([hidden], { x: 3, y: 4 }, settings, bounds)!;
  assert.strictEqual(next[0], hidden);
  assert.equal(next.length, 2);
  assert.equal(next[1].position, 1);
  assert.equal(next[1].channel, 0);
  assert.equal(next[1].slice, 0);
  assert.equal(next[1].frame, 0);
  assert.equal(next[1].pointType, hidden.pointType);
  assert.equal(next[1].size, hidden.size);
  assert.equal(next[1].color, hidden.color);
  assert.equal(next[1].sourceIndex, hidden.sourceIndex);
  const allSlices = addPoint(
    next,
    { x: 5, y: 6 },
    { ...settings, allSlices: true },
    bounds,
  )!;
  assert.equal(allSlices.length, 3);
  assert.strictEqual(allSlices[0], hidden);
  assert.equal(allSlices[2].position, 0);
  assert.equal(allSlices[2].points[0].position, 0);
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
    addPoint([], { x: 1, y: 1 }, { ...settings, counter: 10 }, bounds),
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
