import assert from "node:assert/strict";
import { test } from "node:test";
import {
  pointChoices,
  pointSymbol,
  pointSizes,
  type PointSettings,
} from "./point-settings.ts";

test("point choices vary one property and preserve the other selected properties", () => {
  const selected: PointSettings = {
    type: "Circle",
    color: "Blue",
    size: "Medium",
  };
  for (const key of ["type", "color", "size"] as const) {
    const choices = pointChoices(key, selected);
    assert.equal(choices.filter((choice) => choice.selected).length, 1);
    for (const choice of choices) {
      assert.equal(choice.settings[key], choice.value);
      for (const other of ["type", "color", "size"] as const) {
        if (other !== key)
          assert.equal(choice.settings[other], selected[other]);
      }
    }
  }
  assert.deepEqual(selected, { type: "Circle", color: "Blue", size: "Medium" });
});

test("point icons render distinct geometry and ordered sizes in the selected color", () => {
  const selected: PointSettings = {
    type: "Circle",
    color: "Blue",
    size: "Medium",
  };
  assert.match(pointSymbol(selected), /<circle.*r="5"/);
  assert.match(pointSymbol(selected), /color:#2463eb/);
  assert.doesNotMatch(pointSymbol(selected), /fill="currentColor"/);
  assert.match(
    pointSymbol({ ...selected, type: "Dot" }),
    /fill="currentColor" stroke="none"/,
  );
  assert.match(
    pointSymbol({ ...selected, type: "Cross" }),
    /<path d="M7 12h10M12 7v10"/,
  );
  const radii = Object.values(pointSizes);
  assert.ok(
    radii.every((radius, index) => index === 0 || radius > radii[index - 1]),
  );
});
