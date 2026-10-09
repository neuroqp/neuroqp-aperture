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
  revision: 0,
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

test("events received before the open reply preserve the newest preview", async () => {
  const opening = deferred<ImageState>();
  let notify!: (state: ImageState) => void;
  const seen: ImageState[] = [];
  const loader = new ImageLoader(
    {
      listen: async (changed) => {
        notify = changed;
        return () => {};
      },
      open: () => opening.promise,
      state: async () => null,
      cancel: async () => {},
    },
    (image) => seen.push(image),
    assert.fail,
  );
  const pending = loader.open("new");
  await delay(0);
  notify({ ...state("old"), revision: 20 });
  notify({ ...state("new"), revision: 2 });
  notify({ ...state("new", "loading"), revision: 1, previewReady: false });
  opening.resolve({ ...state("new", "loading"), previewReady: false });
  await pending;
  assert.equal(seen.length, 1);
  assert.equal(seen[0].id, "new");
  assert.equal(seen[0].revision, 2);
  assert.equal(seen[0].previewReady, true);
  loader.dispose();
});

test("preview events update immediately and older in-flight polls cannot undo them", async () => {
  const polling = deferred<ImageState | null>();
  let notify!: (state: ImageState) => void;
  const seen: ImageState[] = [];
  const loader = new ImageLoader(
    {
      listen: async (changed) => {
        notify = changed;
        return () => {};
      },
      open: async () => ({ ...state("new", "loading"), previewReady: false }),
      state: () => polling.promise,
      cancel: async () => {},
    },
    (image) => seen.push(image),
    assert.fail,
  );
  await loader.open("new");
  await delay(275);
  notify({ ...state("new", "loading"), revision: 2 });
  assert.equal(seen.at(-1)?.previewReady, true);
  assert.equal(seen.at(-1)?.revision, 2);
  polling.resolve({
    ...state("new", "loading"),
    revision: 1,
    previewReady: false,
  });
  await delay(0);
  assert.equal(seen.length, 2);
  notify({ ...state("new"), revision: 3 });
  notify({ ...state("new", "loading"), revision: 2 });
  assert.equal(seen.length, 3);
  assert.equal(seen.at(-1)?.phase, "ready");
  loader.dispose();
});

test("cancel, replacement and disposal ignore late native events", async () => {
  let notify!: (state: ImageState) => void;
  let unsubscribed = 0;
  const seen: string[] = [];
  const loader = new ImageLoader(
    {
      listen: async (changed) => {
        notify = changed;
        return () => unsubscribed++;
      },
      open: async (path) => state(path, "loading"),
      state: async () => null,
      cancel: async () => {},
    },
    (image) => seen.push(`${image.id}:${image.phase}`),
    assert.fail,
  );
  await loader.open("old");
  await loader.cancel();
  notify({ ...state("old"), revision: 10 });
  await loader.open("new");
  notify({ ...state("old"), revision: 11 });
  notify({ ...state("new"), revision: 1 });
  loader.dispose();
  notify({ ...state("new", "error"), revision: 2 });
  assert.equal(unsubscribed, 1);
  assert.deepEqual(seen, [
    "old:loading",
    "old:cancelled",
    "new:loading",
    "new:ready",
  ]);
});

test("restore subscribes first and retains an event racing its snapshot", async () => {
  const subscription = deferred<() => void>();
  const snapshot = deferred<ImageState | null>();
  let notify!: (state: ImageState) => void;
  let snapshots = 0;
  const seen: ImageState[] = [];
  const loader = new ImageLoader(
    {
      listen: (changed) => {
        notify = changed;
        return subscription.promise;
      },
      open: async (path) => state(path),
      state: () => {
        snapshots++;
        return snapshot.promise;
      },
      cancel: async () => {},
    },
    (image) => seen.push(image),
    assert.fail,
  );
  const restoring = loader.restore();
  assert.equal(snapshots, 0);
  subscription.resolve(() => {});
  await delay(0);
  assert.equal(snapshots, 1);
  notify({ ...state("restored"), revision: 3 });
  snapshot.resolve({ ...state("restored", "loading"), previewReady: false });
  await restoring;
  assert.equal(seen.length, 1);
  assert.equal(seen[0].revision, 3);
  loader.dispose();
});

test("disposal during subscription unregisters the late listener and prevents open", async () => {
  const subscription = deferred<() => void>();
  let opens = 0;
  let unsubscribed = 0;
  const loader = new ImageLoader(
    {
      listen: () => subscription.promise,
      open: async (path) => {
        opens++;
        return state(path);
      },
      state: async () => null,
      cancel: async () => {},
    },
    () => assert.fail("Disposed loader displayed an image"),
    assert.fail,
  );
  const opening = loader.open("new");
  loader.dispose();
  subscription.resolve(() => unsubscribed++);
  await opening;
  assert.equal(opens, 0);
  assert.equal(unsubscribed, 1);
});

test("fallback polling continues after unchanged snapshots and accepts readiness", async () => {
  const finished = deferred<ImageState>();
  let polls = 0;
  const loader = new ImageLoader(
    {
      open: async () => state("new", "loading"),
      state: async () => {
        polls++;
        return {
          ...state("new", polls < 2 ? "loading" : "ready"),
          revision: polls < 2 ? 0 : 1,
        };
      },
      cancel: async () => {},
    },
    (image) => {
      if (image.phase === "ready") finished.resolve(image);
    },
    assert.fail,
  );
  await loader.open("new");
  try {
    const image = await Promise.race([
      finished.promise,
      delay(1000).then(() => {
        throw new Error("Fallback polling stopped");
      }),
    ]);
    assert.equal(image.revision, 1);
    assert.equal(polls, 2);
  } finally {
    loader.dispose();
  }
});
