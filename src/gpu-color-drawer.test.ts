import assert from "node:assert/strict";
import type OpenSeadragon from "openseadragon";
import test from "node:test";
import { composeColorPixels } from "./color-renderer.ts";
import { createGpuColorDrawer, routeTexture } from "./gpu-color-drawer.ts";
import {
  colorPresets,
  type ColorConnection,
  type ColorSettings,
} from "./color-settings.ts";

const route = (
  source: string,
  color: string,
  minimum = 0,
  maximum = 255,
  brightness = 100,
): ColorConnection => ({ id: 1, source, color, minimum, maximum, brightness });

type Packed = ReturnType<typeof routeTexture>;
// Mirror the shader's texelFetch(ivec2(texel % width, texel / width)).
const texel = ({ data, width }: Packed, index: number) => {
  const x = index % width,
    y = Math.floor(index / width);
  return [...data.subarray((y * width + x) * 4, (y * width + x) * 4 + 4)];
};
// Float32 emulation of the fragment shader's integer-channel path.
function shade(packed: Packed, planes: ArrayLike<number>[], pixel: number) {
  const f = Math.fround;
  const sum = [0, 0, 0];
  for (let route = 0; route < packed.count; route++) {
    const [channel, minimum, maximum, brightness] = texel(packed, 2 * route);
    const color = texel(packed, 2 * route + 1);
    if (channel >= planes.length) continue;
    const ramp = f(f(planes[channel][pixel] - minimum) / f(maximum - minimum));
    const intensity = f(Math.max(0, Math.min(1, ramp)) * brightness);
    for (let component = 0; component < 3; component++)
      sum[component] = f(sum[component] + f(intensity * color[component]));
  }
  return sum.map((value) => Math.round(Math.max(0, Math.min(1, value)) * 255));
}

test("route texture packs every route of 32 inputs × 39 outputs in two-texel rows", () => {
  const colors: Record<string, string> = { ...colorPresets };
  for (let channel = 0; channel < 32; channel++)
    colors[`channel:${channel}`] =
      `#${(channel * 7 + 1).toString(16).padStart(2, "0")}40c0`;
  const names = Object.keys(colors);
  const settings: ColorSettings = {
    colors,
    connections: Array.from({ length: 32 * names.length }, (_, index) => ({
      ...route(
        String(index % 32),
        names[Math.floor(index / 32)],
        index % 1000,
        1000 + index,
        index % 101,
      ),
      id: index + 1,
    })),
  };
  assert.equal(settings.connections.length, 1248);
  for (const width of [1024, 5]) {
    const packed = routeTexture(settings, width);
    assert.equal(packed.count, 1248);
    assert.equal(packed.height, Math.ceil(2496 / width));
    assert.equal(packed.data.length, width * packed.height * 4);
    settings.connections.forEach((connection, index) => {
      const hex = colors[connection.color];
      assert.deepEqual(texel(packed, 2 * index), [
        Number(connection.source),
        connection.minimum,
        connection.maximum,
        Math.fround(connection.brightness / 100),
      ]);
      assert.deepEqual(
        texel(packed, 2 * index + 1),
        [1, 3, 5]
          .map((at) => Math.fround(parseInt(hex.slice(at, at + 2), 16) / 255))
          .concat(0),
      );
    });
  }
  // With an odd width, route 2 ends row 0 and continues at the start of row 1.
  const narrow = routeTexture(settings, 5);
  const at = (x: number, y: number) => [
    ...narrow.data.subarray((y * 5 + x) * 4, (y * 5 + x) * 4 + 4),
  ];
  assert.deepEqual(at(4, 0), [2, 2, 1002, Math.fround(0.02)]);
  assert.deepEqual(at(0, 1), [1, 0, 0, 0]);
  assert.equal(routeTexture({ colors, connections: [] }, 1024).height, 1);
});

test("GPU route math agrees with the CPU composite for 32 channels of 8- and 16-bit samples", () => {
  const settings: ColorSettings = {
    colors: { ...colorPresets, custom: "#1d9e78" },
    connections: Array.from({ length: 32 }, (_, channel) =>
      route(
        String(channel),
        ["Red", "Green", "Blue", "custom"][channel % 4],
        (channel * 5) % 128,
        128 + ((channel * 17) % 128),
        (channel * 19) % 101,
      ),
    ),
  };
  settings.connections.push(
    route("4", "custom", 35, 190),
    route("4", "Blue", 110, 111),
  );
  const packed = routeTexture(settings, 1024);
  for (const scale of [1, 257]) {
    const planes = Array.from({ length: 32 }, (_, channel) =>
      Uint16Array.from(
        { length: 256 },
        (_, value) => ((value + channel * 23) % 256) * scale,
      ),
    );
    const expected = composeColorPixels(planes, settings);
    for (let pixel = 0; pixel < 256; pixel++) {
      const actual = shade(packed, planes, pixel);
      for (let component = 0; component < 3; component++)
        assert.ok(
          Math.abs(actual[component] - expected[pixel * 4 + component]) <= 1,
        );
    }
  }
});

test("adjacent 16-bit values stay distinct at a one-unit range on GPU and CPU", () => {
  const settings: ColorSettings = {
    colors: { ...colorPresets },
    connections: [route("0", "Gray", 1000, 1001)],
  };
  const plane = Uint16Array.of(0, 999, 1000, 1001, 1002, 65535);
  const packed = routeTexture(settings, 1024);
  assert.deepEqual(
    Array.from(plane, (_, pixel) => shade(packed, [plane], pixel)[0]),
    [0, 0, 0, 255, 255, 255],
  );
  assert.deepEqual(
    Array.from(
      composeColorPixels([plane], settings).filter((_, at) => at % 4 === 0),
    ),
    [0, 0, 0, 255, 255, 255],
  );
});

test("a context failure after OSD's support probe falls back without losing cached data", async () => {
  let scheduled: FrameRequestCallback | undefined;
  let invalidated = 0;
  const canvas = {
    getContext: () => null,
    dataset: {} as Record<string, string>,
    removeEventListener() {},
    remove() {},
  };
  const viewer = {
    drawer: { canvas },
    requestDrawer(kind: string) {
      assert.equal(kind, "canvas");
      viewer.drawer = { canvas };
    },
    requestInvalidate(restore: boolean) {
      assert.equal(restore, true);
      invalidated++;
      return Promise.resolve();
    },
  };
  class Base {
    canvas = canvas;
    viewer = viewer;
    destroyInternalCache() {}
    destroy() {}
  }
  const previousFrame = globalThis.requestAnimationFrame;
  const previousCancel = globalThis.cancelAnimationFrame;
  const previousWarn = console.warn;
  try {
    globalThis.requestAnimationFrame = (callback) => {
      scheduled = callback;
      return 1;
    };
    globalThis.cancelAnimationFrame = () => {};
    console.warn = () => {};
    const Drawer = createGpuColorDrawer(
      { DrawerBase: Base } as unknown as typeof OpenSeadragon,
      () => ({ colors: {}, connections: [] }),
      (error) => assert.fail(error),
    );
    const drawer = new Drawer({} as OpenSeadragon.DrawerConstructorParameters);
    assert.ok(scheduled);
    scheduled(0);
    assert.equal(invalidated, 1);
    assert.equal(canvas.dataset.colorRenderer, "cpu");
    assert.doesNotThrow(() => drawer.destroy());
  } finally {
    globalThis.requestAnimationFrame = previousFrame;
    globalThis.cancelAnimationFrame = previousCancel;
    console.warn = previousWarn;
  }
});

function replayHarness() {
  let settings: ColorSettings = {
    colors: { ...colorPresets },
    connections: [route("0", "Gray")],
  };
  const calls = {
    lookups: [] as Float32Array[],
    sourceUploads: 0,
    storage: [] as unknown[][],
    samples: [] as unknown[][],
    selections: 0,
    draws: 0,
    forceRedraws: 0,
  };
  const gl = new Proxy(
    {
      getError: () => 0,
      getShaderParameter: () => true,
      getProgramParameter: () => true,
      getUniformLocation: (_: unknown, name: string) => name,
      getAttribLocation: () => 0,
      getParameter: () => 4096,
      R8UI: "R8UI",
      R16UI: "R16UI",
      RED_INTEGER: "RED_INTEGER",
      UNSIGNED_BYTE: "UNSIGNED_BYTE",
      UNSIGNED_SHORT: "UNSIGNED_SHORT",
      texSubImage2D: (...args: unknown[]) =>
        calls.lookups.push((args.at(-1) as Float32Array).slice()),
      texStorage3D: (...args: unknown[]) => calls.storage.push(args),
      texSubImage3D: (...args: unknown[]) => {
        calls.sourceUploads++;
        calls.samples.push(args);
      },
      drawArrays: () => calls.draws++,
    },
    {
      get(target, name) {
        if (name in target) return target[name as keyof typeof target];
        if (/^[A-Z_0-9]+$/.test(String(name))) return 0;
        return () => (String(name).startsWith("create") ? {} : undefined);
      },
    },
  );
  const canvas = {
    width: 512,
    height: 512,
    dataset: {} as Record<string, string>,
    getContext: () => gl,
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() {},
    remove() {},
  };
  const viewport = {
    getContainerSize: () => ({ x: 512, y: 512 }),
    getBoundsNoRotateWithMargins: () => ({
      width: 1,
      height: 1,
      getCenter: () => ({ x: 0.5, y: 0.5 }),
    }),
    getFlip: () => false,
    getRotation: () => 0,
  };
  const viewer = { forceRedraw: () => calls.forceRedraws++ };
  let data: unknown;
  class Base {
    canvas = canvas;
    viewport = viewport;
    viewer = viewer;
    getDataToDraw() {
      return data;
    }
    destroyInternalCache() {}
    destroy() {}
  }
  class Matrix {
    values: number[];
    constructor(values = []) {
      this.values = values;
    }
    static makeScaling() {
      return new Matrix();
    }
    static makeRotation() {
      return new Matrix();
    }
    static makeTranslation() {
      return new Matrix();
    }
    multiply() {
      return this;
    }
    scaleAndTranslateOtherSetSelf() {}
  }
  const Drawer = createGpuColorDrawer(
    {
      DrawerBase: Base,
      Mat3: Matrix,
      pixelDensityRatio: 1,
    } as unknown as typeof OpenSeadragon,
    () => settings,
    (error) => assert.fail(error),
  );
  const drawer = new Drawer(
    {} as OpenSeadragon.DrawerConstructorParameters,
  ) as OpenSeadragon.DrawerBase & {
    draw(items: OpenSeadragon.TiledImage[]): void;
    refreshColors(): void;
  };
  data = drawer.internalCacheCreate(
    {
      type: "aperturePlanes",
      data: { width: 1, height: 1, bits: 8, planes: [new Uint8Array([128])] },
    } as unknown as OpenSeadragon.CacheRecord,
    {} as OpenSeadragon.Tile,
  );
  const tile = {
    positionedBounds: { x: 0, y: 0, width: 1, height: 1 },
    sourceBounds: { width: 1, height: 1 },
    opacity: 1,
    x: 0,
    y: 0,
    isRightMost: true,
    isBottomMost: true,
  };
  const item = {
    getOpacity: () => 1,
    getRotation: () => 0,
    source: { tileOverlap: 0 },
    getTilesToDraw: () => {
      calls.selections++;
      return [{ tile }];
    },
  };
  drawer.draw([item as unknown as OpenSeadragon.TiledImage]);
  calls.lookups.length = 0;
  calls.sourceUploads = calls.selections = calls.draws = 0;
  return {
    drawer,
    canvas,
    calls,
    data,
    settings: (value: ColorSettings) => {
      settings = value;
    },
  };
}

test("cached recoloring coalesces edits and renders the newest state without tile work", async () => {
  const { drawer, canvas, calls, settings } = replayHarness();
  settings({
    colors: { ...colorPresets },
    connections: [route("0", "Gray", 40, 200)],
  });
  drawer.refreshColors();
  settings({
    colors: { ...colorPresets },
    connections: [route("0", "Gray", 150, 200)],
  });
  drawer.refreshColors();
  assert.equal(calls.draws, 0);
  await Promise.resolve();
  assert.equal(calls.draws, 1);
  assert.equal(calls.lookups.length, 1);
  assert.deepEqual(
    [...calls.lookups[0].subarray(0, 8)],
    [0, 150, 200, 1, 1, 1, 1, 0],
  );
  assert.equal(canvas.dataset.colorRevision, "2");
  assert.equal(calls.selections, 0);
  assert.equal(calls.sourceUploads, 0);
  assert.equal(calls.forceRedraws, 0);
  drawer.destroy();
});

test("freed textures request OSD preparation instead of replaying deleted resources", async () => {
  const { drawer, calls, data } = replayHarness();
  drawer.internalCacheFree(data);
  drawer.refreshColors();
  await Promise.resolve();
  assert.equal(calls.forceRedraws, 1);
  assert.equal(calls.draws, 0);
  assert.equal(calls.lookups.length, 0);
  drawer.destroy();
});

test("destroy cancels a queued color replay", async () => {
  const { drawer, calls } = replayHarness();
  drawer.refreshColors();
  drawer.destroy();
  await Promise.resolve();
  assert.equal(calls.draws, 0);
  assert.equal(calls.forceRedraws, 0);
});

test("brightness refresh updates GPU route data without uploading source textures", async () => {
  const { drawer, calls, settings } = replayHarness();
  settings({
    colors: { ...colorPresets },
    connections: [route("0", "Gray", 0, 255, 50)],
  });
  drawer.refreshColors();
  await Promise.resolve();
  assert.equal(calls.draws, 1);
  assert.equal(calls.lookups[0][3], 0.5);
  assert.equal(calls.sourceUploads, 0);
  assert.equal(calls.selections, 0);
  drawer.destroy();
});

test("channel tiles upload integer textures at their sample width", () => {
  const { drawer, calls } = replayHarness();
  calls.storage.length = calls.samples.length = 0;
  const create = (bits: 8 | 16, plane: Uint8Array | Uint16Array) =>
    drawer.internalCacheCreate(
      {
        type: "aperturePlanes",
        data: { width: 3, height: 1, bits, planes: [plane] },
      } as unknown as OpenSeadragon.CacheRecord,
      {} as OpenSeadragon.Tile,
    );
  const wide = Uint16Array.of(1000, 1001, 65535);
  assert.ok(create(16, wide));
  assert.equal(calls.storage[0][2], "R16UI");
  assert.deepEqual(calls.samples[0].slice(-3), [
    "RED_INTEGER",
    "UNSIGNED_SHORT",
    wide,
  ]);
  assert.ok(create(8, Uint8Array.of(0, 1, 255)));
  assert.equal(calls.storage[1][2], "R8UI");
  assert.equal(calls.samples[1].at(-2), "UNSIGNED_BYTE");
  drawer.destroy();
});
