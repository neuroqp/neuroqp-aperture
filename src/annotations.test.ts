import assert from "node:assert/strict";
import { test } from "node:test";
import {
  addPoint,
  counterCount,
  flipCoordinate,
  imageToStored,
  storedToImage,
  type AnnotationSettings,
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

test("adding points preserves imported metadata and separates changed appearance", () => {
  const first = addPoint([], { x: 100.25, y: 900.5 }, settings, bounds)!;
  first[0].raw = [73, 111, 117, 116];
  const second = addPoint(
    first,
    { x: 111, y: 222 },
    { ...settings, counter: 3 },
    bounds,
  )!;
  assert.equal(first[0].points.length, 1);
  assert.equal(second.length, 1);
  assert.deepEqual(second[0].raw, first[0].raw);
  const third = addPoint(
    second,
    { x: 1, y: 1 },
    { ...settings, color: "Cyan" },
    bounds,
  )!;
  assert.equal(third.length, 2);
  assert.equal(third[0].color, 0xff6c3e89);
  assert.equal(third[1].color, 0xff00bbce);
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
