import assert from "node:assert/strict";
import { test } from "node:test";
import {
  adjustDisplay,
  defaultDisplaySettings as defaults,
} from "./display-settings.ts";

test("display range always retains at least one intensity step", () => {
  const limited = { ...defaults, minimum: 40, maximum: 80 };
  assert.equal(adjustDisplay(limited, "minimum", 90).minimum, 79);
  assert.equal(adjustDisplay(limited, "maximum", 10).maximum, 41);
  assert.equal(adjustDisplay(limited, "minimum", -2).minimum, 0);
  assert.equal(adjustDisplay(limited, "maximum", 999).maximum, 255);
});

test("display adjustments are bounded, independent and leave defaults unchanged", () => {
  assert.deepEqual(adjustDisplay(defaults, "brightness", -120), {
    ...defaults,
    brightness: -100,
  });
  assert.deepEqual(adjustDisplay(defaults, "contrast", 250), {
    ...defaults,
    contrast: 200,
  });
  assert.equal(adjustDisplay(defaults, "brightness", 12.6).brightness, 13);
  assert.equal(adjustDisplay(defaults, "minimum", NaN), defaults);
  assert.equal(adjustDisplay(defaults, "contrast", Infinity), defaults);
  assert.equal(defaults.brightness, 0);
  assert.equal(defaults.contrast, 100);
});
