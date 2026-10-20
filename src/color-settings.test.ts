import assert from "node:assert/strict";
import { test } from "node:test";
import {
  adjustColorConnection,
  colorName,
  colorConnectionValue,
  connectColor,
  copyColorSettings,
  defaultColorSettings,
  normalizeHex,
  removeColorConnection,
  resetColorConnection,
  setColorConnectionRange,
  setOutputColor,
  toHex,
  toHSV,
} from "./color-settings.ts";
import { defaultDisplaySettings } from "./display.ts";

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
  for (const connection of settings.connections) {
    assert.equal(connection.minimum, 0);
    assert.equal(connection.maximum, 255);
    assert.equal(connection.brightness, 100);
  }
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
  assert.equal(extra.connections[2].minimum, 0);
  assert.equal(extra.connections[2].maximum, 255);
  assert.equal(extra.connections[2].brightness, 100);
  const adjusted = setColorConnectionRange(extra, 1, {
    minimum: 50,
    maximum: 150,
  });
  const dimmed = adjustColorConnection(adjusted, 1, "brightness", 35);
  const moved = connectColor(dimmed, "a", "Blue", 1);
  assert.deepEqual(moved.connections[0], {
    ...dimmed.connections[0],
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
  const original = connectColor(
    defaultColorSettings([{ id: "a", name: "A" }]),
    "a",
    "Red",
  );
  const changed = adjustColorConnection(
    adjustColorConnection(original, 1, "minimum", 40.6),
    1,
    "maximum",
    299,
  );
  assert.equal(changed.connections[0].minimum, 41);
  assert.equal(changed.connections[0].maximum, 255);
  assert.equal(changed.connections[1], original.connections[1]);
  assert.equal(original.connections[0].minimum, 0);
  assert.equal(
    adjustColorConnection(changed, 1, "minimum", 999).connections[0].minimum,
    254,
  );
  assert.equal(
    adjustColorConnection(changed, 1, "maximum", -5).connections[0].maximum,
    42,
  );
  assert.equal(
    adjustColorConnection(changed, 1, "minimum", -5).connections[0].minimum,
    0,
  );
  assert.equal(adjustColorConnection(original, 1, "minimum", NaN), original);
  assert.equal(
    adjustColorConnection(original, 1, "maximum", Infinity),
    original,
  );
  const copy = copyColorSettings(original);
  copy.connections[0].source = "other";
  copy.colors.Gray = "#000000";
  assert.equal(original.connections[0].source, "a");
  assert.equal(original.colors.Gray, "#ffffff");
});

test("presets preserve brightness and reset restores only the selected connection", () => {
  const original = connectColor(
    defaultColorSettings([{ id: "a", name: "A" }]),
    "a",
    "Red",
  );
  const dimmed = adjustColorConnection(original, 1, "brightness", 35);
  const first = setColorConnectionRange(dimmed, 1, {
    minimum: 200,
    maximum: 210,
  });
  const preset = setColorConnectionRange(first, 1, {
    minimum: 20,
    maximum: 80,
  });
  assert.deepEqual(preset.connections[0], {
    ...dimmed.connections[0],
    minimum: 20,
    maximum: 80,
  });
  assert.equal(preset.connections[1], original.connections[1]);
  assert.deepEqual(
    setColorConnectionRange(preset, 1, defaultDisplaySettings),
    dimmed,
  );
  assert.deepEqual(resetColorConnection(preset, 1), original);
  assert.equal(
    resetColorConnection(preset, 1).connections[1],
    original.connections[1],
  );
  assert.equal(first.connections[0].minimum, 200);
  for (const range of [
    { minimum: 255, maximum: 255 },
    { minimum: -5, maximum: -1 },
    { minimum: 90, maximum: 10 },
    { minimum: 30.4, maximum: 30.4 },
  ]) {
    const connection = setColorConnectionRange(original, 1, range)
      .connections[0];
    assert.ok(connection.minimum >= 0 && connection.maximum <= 255);
    assert.ok(connection.maximum >= connection.minimum + 1);
  }
  assert.equal(
    setColorConnectionRange(original, 1, { minimum: NaN, maximum: 80 }),
    original,
  );
  assert.equal(
    setColorConnectionRange(original, 1, { minimum: 0, maximum: Infinity }),
    original,
  );
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

test("brightness is bounded, independent and copied with the connection", () => {
  const original = connectColor(
    defaultColorSettings([{ id: "a", name: "A" }]),
    "a",
    "Red",
  );
  const changed = adjustColorConnection(original, 1, "brightness", 35.6);
  assert.equal(changed.connections[0].brightness, 36);
  assert.equal(original.connections[0].brightness, 100);
  assert.equal(changed.connections[1], original.connections[1]);
  assert.equal(
    adjustColorConnection(changed, 1, "brightness", -1).connections[0]
      .brightness,
    0,
  );
  assert.equal(
    adjustColorConnection(changed, 1, "brightness", 101).connections[0]
      .brightness,
    100,
  );
  assert.equal(adjustColorConnection(changed, 1, "brightness", NaN), changed);
  assert.equal(
    adjustColorConnection(changed, 1, "brightness", Infinity),
    changed,
  );
  const copy = copyColorSettings(changed);
  assert.equal(copy.connections[0].brightness, 36);
  copy.connections[0].brightness = 0;
  assert.equal(changed.connections[0].brightness, 36);
});

test("the connection transfer function scales its ramp and clamped plateau", () => {
  const range = { minimum: 50, maximum: 150, brightness: 50 };
  assert.deepEqual(
    [0, 50, 100, 150, 255].map((value) => colorConnectionValue(value, range)),
    [0, 0, 0.25, 0.5, 0.5],
  );
  assert.deepEqual(
    [0, 100, 255].map((value) =>
      colorConnectionValue(value, { ...range, brightness: 0 }),
    ),
    [0, 0, 0],
  );
});

test("single-channel viewing filters display connections without changing adjustments", async () => {
  const { visibleChannelSettings } = await import("./color-settings.ts");
  const settings = defaultColorSettings([
    { id: "0", name: "DAPI" },
    { id: "1", name: "FITC" },
  ]);
  settings.connections[1].brightness = 37;
  const selected = visibleChannelSettings(settings, 1);
  assert.equal(selected.connections.length, 1);
  assert.equal(selected.connections[0].source, "1");
  assert.equal(selected.connections[0].brightness, 37);
  assert.equal(settings.connections.length, 2);
  assert.equal(visibleChannelSettings(settings, -1), settings);
});

test("color layouts initialize after empty metadata and reset when channel identity changes", async () => {
  const { sameColorChannels } = await import("./color-settings.ts");
  const channels = [
    { id: "0", name: "DAPI", color: "#0000ff" },
    { id: "1", name: "FITC", color: "#00ff00" },
  ];
  assert.equal(sameColorChannels([], channels), false);
  assert.equal(
    sameColorChannels(
      channels,
      channels.map((channel) => ({ ...channel })),
    ),
    true,
  );
  assert.equal(sameColorChannels(channels, channels.slice(0, 1)), false);
  assert.equal(sameColorChannels(channels, [...channels].reverse()), false);
  for (const replacement of [
    { id: "2" },
    { name: "TRITC" },
    { color: "#ff0000" },
  ])
    assert.equal(
      sameColorChannels(channels, [
        { ...channels[0], ...replacement },
        channels[1],
      ]),
      false,
    );
});
