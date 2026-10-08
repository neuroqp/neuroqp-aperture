import assert from "node:assert/strict";
import { test } from "node:test";
import {
  hitPoint,
  pointAction,
  keyPlacementAllowed,
} from "./point-interaction.ts";

test("hit testing has a fixed inclusive five CSS-pixel radius and favors nearest then topmost centers", () => {
  const points = [
    { roiIndex: 0, pointIndex: 0, x: 100, y: 200 },
    { roiIndex: 1, pointIndex: 1, x: 107, y: 200 },
    { roiIndex: 1, pointIndex: 2, x: 100, y: 200 },
  ];
  assert.strictEqual(hitPoint(points, { x: 100, y: 200 }), points[2]);
  assert.strictEqual(hitPoint(points, { x: 103, y: 204 }), points[2]);
  assert.strictEqual(hitPoint(points, { x: 105, y: 200 }), points[1]);
  assert.strictEqual(hitPoint([points[0]], { x: 105, y: 200 }), points[0]);
  assert.equal(hitPoint([points[0]], { x: 105.0001, y: 200 }), null);
  assert.equal(hitPoint([points[0]], { x: 109, y: 200 }), null);
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

test("E placement requires editable multi-point hover and ignores repeat, modifiers, controls, and pointer gestures", () => {
  const allowed = {
    active: true,
    editable: true,
    hovering: true,
    repeat: false,
    modified: false,
    pressed: false,
    typing: false,
  };
  assert.equal(keyPlacementAllowed(allowed), true);
  for (const key of ["active", "editable", "hovering"] as const)
    assert.equal(keyPlacementAllowed({ ...allowed, [key]: false }), false);
  for (const key of ["repeat", "modified", "pressed", "typing"] as const)
    assert.equal(keyPlacementAllowed({ ...allowed, [key]: true }), false);
});
