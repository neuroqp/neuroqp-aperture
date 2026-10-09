import assert from "node:assert/strict";
import test from "node:test";
import { composeColorPixels } from "./color-renderer.ts";
import {
  colorPresets,
  type ColorConnection,
  type ColorSettings,
} from "./color-settings.ts";

const route = (
  source: string,
  color: string,
  adjustment: Partial<ColorConnection> = {},
): ColorConnection => ({
  id: 1,
  source,
  color,
  minimum: 0,
  maximum: 255,
  ...adjustment,
});
const settings = (...connections: ColorConnection[]): ColorSettings => ({
  colors: { ...colorPresets },
  connections,
});

test("ordinary RGB and grayscale defaults preserve display bytes and source planes", () => {
  const rgb = [
    new Uint8Array([0, 31, 255]),
    new Uint8Array([51, 128, 0]),
    new Uint8Array([255, 3, 72]),
  ];
  const before = rgb.map((plane) => plane.slice());
  assert.deepEqual(
    [
      ...composeColorPixels(
        rgb,
        settings(route("0", "Red"), route("1", "Green"), route("2", "Blue")),
        new Uint8Array([255, 0, 127]),
      ),
    ],
    [0, 51, 255, 255, 31, 128, 3, 0, 255, 0, 72, 127],
  );
  assert.deepEqual(rgb, before);
  assert.deepEqual(
    [
      ...composeColorPixels(
        [new Uint8Array([0, 1, 128, 255])],
        settings(route("0", "Gray")),
      ),
    ],
    [0, 0, 0, 255, 1, 1, 1, 255, 128, 128, 128, 255, 255, 255, 255, 255],
  );
});

test("many-to-many routes include channels beyond RGB and clamp after additive composition", () => {
  const planes = [
    new Uint8Array([80]),
    new Uint8Array([20]),
    new Uint8Array([30]),
    new Uint8Array([200]),
  ];
  const display = settings(
    route("0", "Red"),
    route("0", "Blue"),
    route("1", "Green"),
    route("3", "Red"),
  );
  assert.deepEqual(
    [...composeColorPixels(planes, display)],
    [255, 20, 80, 255],
  );
  display.connections = display.connections.filter(
    (connection) => connection.source !== "3",
  );
  assert.deepEqual([...composeColorPixels(planes, display)], [80, 20, 80, 255]);
  assert.deepEqual([...composeColorPixels(planes, settings())], [0, 0, 0, 255]);
});

test("route display ranges are independent and clip before color mixing", () => {
  const plane = new Uint8Array([0, 64, 128, 192, 255]);
  const display = settings(
    route("0", "Red", { minimum: 64, maximum: 192 }),
    route("0", "Green", { minimum: 0, maximum: 128 }),
    route("0", "Blue", { minimum: 128, maximum: 255 }),
  );
  const pixels = composeColorPixels([plane], display);
  assert.deepEqual(
    [...pixels],
    [
      0, 0, 0, 255, 0, 128, 0, 255, 128, 255, 0, 255, 255, 255, 129, 255, 255,
      255, 255, 255,
    ],
  );
  display.colors.Red = "#102030";
  assert.deepEqual(
    [
      ...composeColorPixels(
        [new Uint8Array([255])],
        settings(route("0", "Red")),
      ),
    ],
    [255, 0, 0, 255],
  );
  assert.deepEqual(
    [
      ...composeColorPixels([new Uint8Array([255])], {
        colors: { tint: "#102030" },
        connections: [route("0", "tint")],
      }),
    ],
    [16, 32, 48, 255],
  );
});

test("clipping precedes tinting and additive mixing, and reset restores identity", async () => {
  const { defaultDisplaySettings } = await import("./display-settings.ts");
  const { setColorConnectionRange } = await import("./color-settings.ts");
  const display: ColorSettings = {
    colors: { tint: "#102030" },
    connections: [route("0", "tint", { minimum: 50, maximum: 150 })],
  };
  const plane = new Uint8Array([0, 50, 100, 150, 255]);
  assert.deepEqual(
    [...composeColorPixels([plane], display)],
    [
      0, 0, 0, 255, 0, 0, 0, 255, 8, 16, 24, 255, 16, 32, 48, 255, 16, 32, 48,
      255,
    ],
  );
  const overlapping = settings(
    route("0", "Red", { minimum: 50, maximum: 150 }),
    route("0", "Yellow", { minimum: 0, maximum: 200 }),
  );
  assert.deepEqual(
    [...composeColorPixels([new Uint8Array([255])], overlapping)],
    [255, 255, 0, 255],
  );
  const reset = setColorConnectionRange(display, 1, defaultDisplaySettings);
  assert.deepEqual(
    [...composeColorPixels([new Uint8Array([255])], reset)],
    [16, 32, 48, 255],
  );
  assert.equal(display.connections[0].minimum, 50);
});

test("mismatched channel dimensions fail instead of silently misaligning channels", () => {
  assert.throws(
    () =>
      composeColorPixels([new Uint8Array(1), new Uint8Array(2)], settings()),
    /dimensions differ/,
  );
});
