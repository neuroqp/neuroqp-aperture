import assert from "node:assert/strict";
import { test } from "node:test";
import {
  adjustColorConnection,
  colorName,
  connectColor,
  copyColorSettings,
  defaultColorSettings,
  normalizeHex,
  removeColorConnection,
  setOutputColor,
  toHex,
  toHSV,
} from "./color-settings.ts";

test("default connections retain RGB and grayscale identity with metadata channel IDs", () => {
  const settings = defaultColorSettings([
    { id: "r", name: "Red", color: "#ff0000" },
    { id: "b", name: "Blue", color: "Blue" },
    { id: "gray", name: "Intensity" },
  ]);
  assert.deepEqual(
    settings.connections.map(({ source, color }) => [source, color]),
    [
      ["r", "Red"],
      ["b", "Blue"],
      ["gray", "Gray"],
    ],
  );
  assert.equal(settings.colors.Gray, "#ffffff");
  assert.deepEqual(defaultColorSettings([]).connections, []);
  const custom = defaultColorSettings([
    { id: "x", name: "Metadata color", color: "#234567" },
  ]);
  assert.equal(custom.colors[custom.connections[0].color], "#234567");
});

test("many-to-many connections move only the output and preserve other edges", () => {
  const original = defaultColorSettings([
    { id: "a", name: "A", color: "Red" },
    { id: "b", name: "B", color: "Green" },
  ]);
  const extra = connectColor(connectColor(original, "a", "Green"), "b", "Red");
  assert.equal(extra.connections.length, 4);
  const moved = connectColor(extra, "a", "Blue", 1);
  assert.deepEqual(moved.connections[0], {
    ...extra.connections[0],
    color: "Blue",
  });
  assert.deepEqual(moved.connections.slice(1), extra.connections.slice(1));
  assert.equal(connectColor(extra, "a", "Green", 1), extra);
  assert.equal(connectColor(extra, "b", "Blue", 1), extra);
  assert.equal(connectColor(extra, "a", "missing"), extra);
  assert.equal(removeColorConnection(moved, 1).connections.length, 3);
  assert.equal(original.connections.length, 2);
});

test("connection adjustments and snapshots cannot mutate the original state", () => {
  const original = defaultColorSettings([{ id: "a", name: "A" }]);
  const changed = adjustColorConnection(
    adjustColorConnection(original, 1, "brightness", -200),
    1,
    "contrast",
    299,
  );
  assert.equal(changed.connections[0].brightness, -100);
  assert.equal(changed.connections[0].contrast, 200);
  assert.equal(adjustColorConnection(original, 1, "brightness", NaN), original);
  const copy = copyColorSettings(original);
  copy.connections[0].source = "other";
  copy.colors.Gray = "#000000";
  assert.equal(original.connections[0].source, "a");
  assert.equal(original.colors.Gray, "#ffffff");
});

test("picker accepts hex, names by hue, and round trips colors", () => {
  assert.equal(normalizeHex(" AbC "), "#aabbcc");
  assert.equal(normalizeHex("#zz0000"), null);
  for (const hex of ["#ff0000", "#000000", "#ffffff", "#234567", "#d4358a"])
    assert.equal(toHex(toHSV(hex)), hex);
  assert.equal(colorName("#ff0000"), "Red");
  assert.equal(colorName("#ffffff"), "Gray");
  assert.equal(colorName("#808080"), "Gray");
  assert.equal(colorName("#c02020"), "Red-ish");
  assert.equal(colorName("#987654"), "Yellow-ish");
  assert.equal(colorName("#777779"), "Gray-ish");
  const settings = defaultColorSettings([]);
  assert.equal(setOutputColor(settings, "Red", "bad color"), settings);
  assert.equal(setOutputColor(settings, "Red", "#abc").colors.Red, "#aabbcc");
  assert.equal(settings.colors.Red, "#ff0000");
});
