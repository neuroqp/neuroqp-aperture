import assert from "node:assert/strict";
import { test } from "node:test";
import {
  adjustDisplay,
  channelHistograms,
  defaultDisplaySettings as defaults,
  component,
  splitChannels,
} from "./display.ts";

test("RGBA pixels split into one plane per requested component", () => {
  const rgba = Uint8ClampedArray.of(1, 2, 3, 4, 5, 6, 7, 8);
  assert.deepEqual(
    splitChannels(rgba, 4).map((plane) => [...plane]),
    [
      [1, 5],
      [2, 6],
      [3, 7],
      [4, 8],
    ],
  );
  assert.deepEqual([...component(rgba, 3)], [4, 8]);
});

test("channel histograms retain independent original samples and ignore transparency", () => {
  const planes = [
    Uint8Array.of(10, 10, 80, 200),
    Uint8Array.of(90, 90, 0, 250),
  ];
  const bins = channelHistograms(planes, 2, 2, [255, 255, 255, 0]);
  assert.equal(bins[0][10], 2);
  assert.equal(bins[0][80], 1);
  assert.equal(bins[0][200], 0);
  assert.equal(bins[1][90], 2);
  assert.equal(bins[1][0], 1);
  assert.deepEqual([...planes[0]], [10, 10, 80, 200]);
  const large = channelHistograms(
    [new Uint8Array(1024 * 1024).fill(42)],
    1024,
    1024,
  );
  assert.equal(large[0][42], 256 * 256);
});

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
  const { displayValue } = await import("./display.ts");
  for (let value = 0; value < 256; value++)
    assert.equal(displayValue(value, defaults), value / 255);
  const range = { minimum: 50, maximum: 150 };
  assert.equal(displayValue(0, range), 0);
  assert.equal(displayValue(50, range), 0);
  assert.equal(displayValue(100, range), 0.5);
  assert.equal(displayValue(150, range), 1);
  assert.equal(displayValue(255, range), 1);
});

test("percentile presets clip the requested tails and handle uniform and empty images", async () => {
  const { percentileRange } = await import("./display.ts");
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
  const { percentileRange } = await import("./display.ts");
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
