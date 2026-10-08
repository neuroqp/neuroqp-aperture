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

test("display adjustments round, ignore invalid values and leave defaults unchanged", () => {
  assert.equal(adjustDisplay(defaults, "minimum", 12.6).minimum, 13);
  assert.equal(adjustDisplay(defaults, "minimum", NaN), defaults);
  assert.equal(adjustDisplay(defaults, "maximum", Infinity), defaults);
  assert.deepEqual(defaults, { minimum: 0, maximum: 255 });
});

test("display transfer clips the selected range and resets to identity", async () => {
  const { displayValue, displayFilter } = await import("./display-settings.ts");
  for (const minimum of [0, 1, 50, 127, 254]) {
    for (const maximum of [minimum + 1, 255]) {
      const range = { minimum, maximum };
      const factors = [
        ...displayFilter(range).matchAll(/\(([0-9.e+-]+)\)/g),
      ].map((m) => Number(m[1]));
      const clamp = (x: number) => Math.max(0, Math.min(1, x));
      for (let value = 0; value < 256; value++) {
        const filtered = clamp(
          (clamp((value / 255) * factors[0]) - 0.5) * factors[1] + 0.5,
        );
        assert.ok(Math.abs(filtered - displayValue(value, range)) < 1e-12);
      }
    }
  }
  for (let value = 0; value < 256; value++)
    assert.equal(displayValue(value, defaults), value / 255);
  const range = { minimum: 50, maximum: 150 };
  assert.equal(displayValue(0, range), 0);
  assert.equal(displayValue(50, range), 0);
  assert.equal(displayValue(100, range), 0.5);
  assert.equal(displayValue(150, range), 1);
  assert.equal(displayValue(255, range), 1);
});

test("histogram counts original preview luminance and ignores transparent pixels", async () => {
  const { histogram } = await import("./display-settings.ts");
  const pixels = new Uint8ClampedArray([
    0, 0, 0, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255,
    255, 90, 90, 90, 0,
  ]);
  const before = pixels.slice();
  const bins = histogram(pixels);
  assert.equal(bins.length, 256);
  assert.equal(
    bins.reduce((sum, n) => sum + n, 0),
    5,
  );
  for (const value of [0, 255, 54, 182, 18]) assert.equal(bins[value], 1);
  assert.deepEqual(pixels, before);
});

test("percentile presets clip the requested tails and handle uniform and empty images", async () => {
  const { percentileRange } = await import("./display-settings.ts");
  const bins = Array(256).fill(0);
  bins[0] = 10;
  bins[20] = 40;
  bins[40] = 50;
  bins[80] = 9800;
  bins[150] = 50;
  bins[200] = 40;
  bins[255] = 10;
  assert.deepEqual(percentileRange(bins, 0.1), { minimum: 0, maximum: 200 });
  assert.deepEqual(percentileRange(bins, 0.5), { minimum: 20, maximum: 150 });
  assert.deepEqual(percentileRange(bins, 1), { minimum: 40, maximum: 80 });
  for (const value of [0, 80, 255]) {
    const constant = Array(256).fill(0);
    constant[value] = 100;
    const result = percentileRange(constant, 1);
    assert.ok(result.maximum > result.minimum);
    assert.ok(result.minimum >= 0 && result.maximum <= 255);
  }
  assert.deepEqual(percentileRange(Array(256).fill(0), 1), {
    minimum: 0,
    maximum: 255,
  });
});

test("0–100 uses occupied histogram endpoints without clipping rare intensities", async () => {
  const { percentileRange } = await import("./display-settings.ts");
  const bins = Array(256).fill(0);
  bins[30] = 1;
  bins[100] = 10000;
  bins[210] = 1;
  assert.deepEqual(percentileRange(bins, 0), { minimum: 30, maximum: 210 });
  for (const value of [0, 80, 255]) {
    const constant = Array(256).fill(0);
    constant[value] = 1;
    const result = percentileRange(constant, 0);
    assert.ok(result.maximum > result.minimum);
    assert.ok(result.minimum >= 0 && result.maximum <= 255);
  }
  assert.deepEqual(percentileRange(Array(256).fill(0), 0), defaults);
});
