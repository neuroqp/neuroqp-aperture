import assert from "node:assert/strict";
import type OpenSeadragon from "openseadragon";
import test from "node:test";
import { composeColorPixels } from "./color-renderer.ts";
import { createGpuColorDrawer, gpuColorLookup } from "./gpu-color-drawer.ts";
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
): ColorConnection => ({ id: 1, source, color, minimum, maximum });

test("GPU lookup keeps clipped route sums above one until final composition", () => {
  const settings: ColorSettings = {
    colors: { ...colorPresets },
    connections: [
      route("0", "Red", 50, 150),
      route("0", "Yellow", 0, 200),
      route("31", "Blue", 128, 255),
    ],
  };
  const lookup = gpuColorLookup(settings);
  assert.equal(lookup.length, 256 * 32 * 4);
  assert.equal(lookup[255 * 4], 2);
  assert.equal(lookup[255 * 4 + 1], 1);
  assert.equal(lookup[255 * 4 + 2], 0);
  assert.equal(lookup[0], 0);
  assert.equal(lookup[100 * 4], 1);
  assert.equal(lookup[100 * 4 + 1], 0.5);
  assert.equal(lookup[(31 * 256 + 255) * 4 + 2], 1);
  assert.equal(lookup[31 * 256 * 4 + 2], 0);
});

test("GPU float lookup agrees with CPU for all intensities and 32-channel routes", () => {
  const settings: ColorSettings = {
    colors: { ...colorPresets, custom: "#1d9e78" },
    connections: Array.from({ length: 32 }, (_, channel) =>
      route(
        String(channel),
        ["Red", "Green", "Blue", "custom"][channel % 4],
        (channel * 5) % 128,
        128 + ((channel * 17) % 128),
      ),
    ),
  };
  settings.connections.push(
    route("4", "custom", 35, 190),
    route("4", "Blue", 110, 111),
  );
  const planes = Array.from({ length: 32 }, (_, channel) =>
    Uint8Array.from(
      { length: 256 },
      (_, value) => (value + channel * 23) % 256,
    ),
  );
  const before = planes.map((plane) => plane.slice());
  const expected = composeColorPixels(planes, settings);
  const lookup = gpuColorLookup(settings);
  for (let value = 0; value < 256; value++) {
    for (let component = 0; component < 3; component++) {
      let sum = 0;
      for (let channel = 0; channel < 32; channel++)
        sum = Math.fround(
          sum +
            lookup[(channel * 256 + planes[channel][value]) * 4 + component],
        );
      const actual = Math.round(Math.max(0, Math.min(1, sum)) * 255);
      assert.ok(Math.abs(actual - expected[value * 4 + component]) <= 1);
    }
  }
  assert.deepEqual(planes, before);
});

test("changing settings builds only a new small lookup, preserving disconnected black", () => {
  const settings: ColorSettings = {
    colors: { ...colorPresets },
    connections: [route("0", "Gray")],
  };
  const original = gpuColorLookup(settings, 1);
  settings.connections = [];
  assert.ok(gpuColorLookup(settings, 1).every((value) => value === 0));
  assert.equal(original[255 * 4], 1);
  assert.equal(original[128 * 4 + 1], Math.fround(128 / 255));
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
      texSubImage2D: (...args: unknown[]) =>
        calls.lookups.push((args.at(-1) as Float32Array).slice()),
      texSubImage3D: () => calls.sourceUploads++,
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
      data: { width: 1, height: 1, planes: [new Uint8Array([128])] },
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
  assert.equal(calls.lookups[0][128 * 4], 0);
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
