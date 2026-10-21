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

test("16-bit histograms keep every source value and presets stay in the domain", async () => {
  const { percentileRange } = await import("./display.ts");
  const wide: [number, number] = [0, 65535];
  const plane = Uint16Array.of(1000, 1001, 1001, 40000);
  const [bins] = channelHistograms([plane], 2, 2, undefined, 16);
  assert.equal(bins.length, 65536);
  assert.equal(bins[1000], 1);
  assert.equal(bins[1001], 2);
  assert.equal(bins[40000], 1);
  assert.deepEqual(percentileRange(bins, 0, wide), {
    minimum: 1000,
    maximum: 40000,
  });
  assert.deepEqual(percentileRange(bins, 30, wide), {
    minimum: 1001,
    maximum: 1001 + 1,
  });
  const constant = new Uint32Array(65536);
  constant[300] = 9;
  assert.deepEqual(percentileRange(constant, 0, wide), {
    minimum: 300,
    maximum: 301,
  });
  constant.fill(0)[65535] = 9;
  assert.deepEqual(percentileRange(constant, 0, wide), {
    minimum: 65534,
    maximum: 65535,
  });
  assert.deepEqual(percentileRange(new Uint32Array(65536), 1, wide), {
    minimum: 0,
    maximum: 65535,
  });
});

test("display ranges clamp to the channel domain", () => {
  const wide: [number, number] = [0, 65535];
  const range = { minimum: 1000, maximum: 1001 };
  assert.equal(adjustDisplay(range, "maximum", 70000, wide).maximum, 65535);
  assert.equal(adjustDisplay(range, "minimum", 5000, wide).minimum, 1000);
  assert.equal(adjustDisplay(range, "maximum", 2, wide).maximum, 1001);
  assert.equal(adjustDisplay(range, "minimum", -9, wide).minimum, 0);
  assert.equal(adjustDisplay(defaults, "maximum", 999).maximum, 255);
});

test("the histogram axis keeps dim 16-bit data readable and 8-bit unchanged", async () => {
  const { displayBins, histogramAxis } = await import("./display.ts");
  const bins = new Uint32Array(65536);
  bins[10] = 5;
  bins[3000] = 7;
  assert.deepEqual(histogramAxis(bins, [0, 65535], 3000), [0, 4095]);
  assert.deepEqual(histogramAxis(bins, [0, 65535], 9000), [0, 16383]);
  assert.deepEqual(histogramAxis(bins, [0, 65535], 65535), [0, 65535]);
  assert.deepEqual(histogramAxis(null, [0, 65535], 1), [0, 255]);
  const narrow = new Uint32Array(256);
  narrow[3] = 1;
  assert.deepEqual(histogramAxis(narrow, [0, 255], 4), [0, 255]);
  const shown = displayBins(bins, [0, 4095]);
  assert.equal(shown.length, 256);
  assert.equal(shown[0], 5);
  assert.equal(shown[Math.floor(3000 / 16)], 7);
  assert.equal(
    shown.reduce((sum, count) => sum + count, 0),
    12,
  );
  assert.deepEqual([...displayBins(narrow, [0, 255])], [...narrow]);
});
