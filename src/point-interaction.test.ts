import assert from "node:assert/strict";
import { test } from "node:test";
import { hitPoint, pointAction } from "./point-interaction.ts";

test("hit testing uses screen-space marker sizes, nearest center, and topmost overlap", () => {
  const points = [
    { roiIndex: 0, pointIndex: 0, x: 100, y: 200, radius: 2 },
    { roiIndex: 1, pointIndex: 1, x: 130, y: 200, radius: 16 },
    { roiIndex: 1, pointIndex: 2, x: 100, y: 200, radius: 5 },
  ];
  assert.strictEqual(hitPoint(points, { x: 100, y: 200 }), points[2]);
  assert.strictEqual(hitPoint(points, { x: 108, y: 200 }), points[2]);
  assert.strictEqual(hitPoint(points, { x: 119, y: 200 }), points[1]);
  assert.equal(hitPoint(points, { x: 150, y: 200 }), null);
  assert.equal(hitPoint([], { x: 100, y: 200 }), null);
});

test("Space or Control pan has priority over point deletion and movement in every tool", () => {
  for (const active of [false, true]) {
    for (const alt of [false, true]) {
      for (const hit of [false, true])
        assert.equal(pointAction(active, true, alt, hit), "pan");
    }
  }
  assert.equal(pointAction(false, false, true, true), "pan");
  assert.equal(pointAction(true, false, true, true), "delete");
  assert.equal(pointAction(true, false, false, true), "move");
  assert.equal(pointAction(true, false, false, false), "place");
  assert.equal(pointAction(true, false, true, false), "none");
});
