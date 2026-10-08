import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import {
  ImageLoader,
  orientation,
  preview,
  pyramid,
  tileFailure,
  type ImageState,
} from "./image.ts";

const state = (
  id: string,
  phase: ImageState["phase"] = "ready",
): ImageState => ({
  id,
  name: `${id}.tif`,
  width: 10001,
  height: 7777,
  phase,
  previewReady: true,
  tilesReady: true,
  progress: null,
  error: null,
});
const deferred = <T>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
};

test("preview and pyramid use identical original bounds and DZI level geometry", () => {
  const image = state("a");
  const source = pyramid(image, (path) => `aperture://${path}`);
  const thumbnail = preview(image, (path) => `aperture://${path}`);
  assert.equal(source.maxLevel, 14);
  assert.equal(source.minLevel, 11);
  assert.ok(image.width * 2 ** (source.minLevel - source.maxLevel) >= 1024);
  assert.equal(source.tileSize, 256);
  assert.equal(source.tileOverlap, 0);
  assert.equal(
    source.getTileUrl(14, 39, 30),
    "aperture://a/tiles/14/39_30.jpg",
  );
  assert.equal(thumbnail.width, source.width);
  assert.equal(thumbnail.height, source.height);
  assert.equal(thumbnail.maxLevel, 0);
  assert.equal(thumbnail.tileSize, image.width);
  assert.equal(thumbnail.getTileUrl(), "aperture://a/preview.jpg");
  const convert = (id: string) =>
    `aperture://localhost/${encodeURIComponent(id)}`;
  assert.equal(
    preview(image, convert).getTileUrl(),
    "aperture://localhost/a/preview.jpg",
  );
  assert.equal(
    pyramid(image, convert).getTileUrl(14, 39, 30),
    "aperture://localhost/a/tiles/14/39_30.jpg",
  );
  assert.equal(pyramid({ ...image, width: 1, height: 1 }, String).maxLevel, 0);
  assert.equal(
    pyramid({ ...image, width: 256, height: 257 }, String).maxLevel,
    9,
  );
});

test("all EXIF orientations map stored pixels to the correct displayed position", () => {
  const expected = [
    [1, 2],
    [-1, 2],
    [-1, -2],
    [1, -2],
    [2, 1],
    [-2, 1],
    [-2, -1],
    [2, -1],
  ];
  for (let value = 1; value <= 8; value++) {
    const { degrees, flipped } = orientation(value);
    const angle = (degrees * Math.PI) / 180;
    const x = flipped ? -1 : 1;
    const y = 2;
    assert.deepEqual(
      [
        Math.round(x * Math.cos(angle) - y * Math.sin(angle)),
        Math.round(x * Math.sin(angle) + y * Math.cos(angle)),
      ],
      expected[value - 1],
      `EXIF orientation ${value}`,
    );
  }
  assert.deepEqual(orientation(), { degrees: 0, flipped: false });
  assert.deepEqual(orientation(99), { degrees: 0, flipped: false });
});

test("tile failures report only the active image and ignore cancellation and stale requests", () => {
  const base = "aperture://localhost/123";
  assert.equal(
    tileFailure(base, "ready", `${base}/tiles/4/0_0.jpg`),
    "Detail could not load. Preview remains available.",
  );
  assert.equal(
    tileFailure(base, "loading", `${base}/preview.jpg`),
    "Could not display the image preview.",
  );
  assert.equal(tileFailure(base, "cancelled", `${base}/tiles/4/0_0.jpg`), null);
  assert.equal(
    tileFailure(base, "ready", "aperture://localhost/1234/tiles/4/0_0.jpg"),
    null,
  );
  assert.equal(tileFailure(null, "loading", `${base}/preview.jpg`), null);
  assert.equal(tileFailure(base, "error", `${base}/preview.jpg`), null);
});

test("a late open reply cannot replace a newer image", async () => {
  const old = deferred<ImageState>();
  const seen: string[] = [];
  const loader = new ImageLoader(
    {
      open: (path) =>
        path === "old" ? old.promise : Promise.resolve(state(path)),
      state: async () => null,
      cancel: async () => {},
    },
    (image) => seen.push(image.id),
    assert.fail,
  );
  const opening = loader.open("old");
  await loader.open("new");
  old.resolve(state("old"));
  await opening;
  assert.deepEqual(seen, ["new"]);
  loader.dispose();
});

test("cancel while opening is pending finishes UI cancellation and discards its reply", async () => {
  const pending = deferred<ImageState>();
  const seen: string[] = [];
  const loader = new ImageLoader(
    {
      open: () => pending.promise,
      state: async () => null,
      cancel: async () => {},
    },
    (image) => seen.push(image.id),
    assert.fail,
  );
  const opening = loader.open("old");
  assert.equal(await loader.cancel(), true);
  pending.resolve(state("old", "loading"));
  await opening;
  assert.deepEqual(seen, []);
  loader.dispose();
});

test("in-flight poll and cancellation cannot overwrite a newer image", async () => {
  const poll = deferred<ImageState | null>();
  const cancel = deferred<void>();
  const seen: string[] = [];
  const loader = new ImageLoader(
    {
      open: async (path) => state(path, path === "old" ? "loading" : "ready"),
      state: () => poll.promise,
      cancel: () => cancel.promise,
    },
    (image) => seen.push(`${image.id}:${image.phase}`),
    assert.fail,
  );
  await loader.open("old");
  await delay(275);
  const cancelling = loader.cancel();
  await loader.open("new");
  poll.resolve(state("old"));
  cancel.resolve();
  assert.equal(await cancelling, false);
  await delay(0);
  assert.deepEqual(seen, ["old:loading", "new:ready"]);
  loader.dispose();
});

test("poll detects a mismatched native load and reports backend errors", async () => {
  const failures: string[] = [];
  const loader = new ImageLoader(
    {
      open: async () => state("old", "loading"),
      state: async () => state("different"),
      cancel: async () => {},
    },
    () => {},
    (error) => failures.push(error),
  );
  await loader.open("old");
  await delay(275);
  assert.deepEqual(failures, ["The image load is no longer available."]);
  loader.dispose();
});
