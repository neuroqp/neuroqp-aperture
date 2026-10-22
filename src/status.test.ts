import assert from "node:assert/strict";
import { test } from "node:test";
import type { ImageState } from "./image.ts";
import { statusFor, type StatusInput } from "./status.ts";

const image = (patch: Partial<ImageState> = {}): ImageState => ({
  id: "a",
  revision: 0,
  name: "a.tif",
  width: 100,
  height: 80,
  phase: "ready",
  previewReady: true,
  tilesReady: true,
  progress: null,
  error: null,
  sampleBits: 8,
  rgb: false,
  ...patch,
});
const status = (patch: Partial<StatusInput> = {}) =>
  statusFor({
    image: image(),
    message: null,
    saving: false,
    saveError: null,
    saveNotice: null,
    savedPath: null,
    dirty: false,
    annotationError: null,
    tileError: null,
    ...patch,
  });

test("loading progress and browsing-only images describe the image state", () => {
  assert.deepEqual(status({ image: null }), {
    text: "",
    title: "",
    error: false,
  });
  assert.equal(
    status({
      image: image({ phase: "loading", previewReady: false, width: 0 }),
    }).text,
    "Reading image…",
  );
  assert.equal(
    status({ image: image({ phase: "loading", previewReady: false }) }).text,
    "Building preview…",
  );
  assert.deepEqual(
    status({ image: image({ phase: "loading", detail: "CZI · 3 channels" }) }),
    { text: "Loading detail…", title: "CZI · 3 channels", error: false },
  );
  assert.equal(
    status({ image: image({ readOnly: true }) }).text,
    "Browsing only · annotations unavailable",
  );
  assert.equal(
    status({ image: image({ displayFallback: true }) }).text,
    "8-bit display · image too large for 16-bit cache",
  );
});

test("errors outrank progress and persist until the image changes", () => {
  const loading = image({ phase: "loading" });
  assert.deepEqual(
    status({ image: loading, tileError: "Detail could not load." }),
    {
      text: "Detail could not load.",
      title: "Detail could not load.",
      error: true,
    },
  );
  assert.equal(
    status({
      image: loading,
      tileError: "Detail could not load.",
      annotationError: "Annotations could not load: denied",
    }).text,
    "Annotations could not load: denied",
  );
  assert.equal(
    status({
      image: image({ phase: "error", error: "Unsupported file" }),
      annotationError: "Annotations could not load: denied",
    }).text,
    "Unsupported file",
  );
  assert.equal(
    status({ image: image({ phase: "error" }) }).text,
    "Could not load image",
  );
});

test("cancellation hides display failures but not annotation failures", () => {
  const cancelled = image({ phase: "cancelled" });
  assert.deepEqual(status({ image: cancelled, tileError: "Detail failed." }), {
    text: "Cancelled",
    title: "Cancelled",
    error: false,
  });
  assert.equal(
    status({ image: cancelled, annotationError: "Annotations failed." }).text,
    "Annotations failed.",
  );
});

test("save failures outrank every other message and saving outranks image errors", () => {
  assert.deepEqual(
    status({
      saveError: "Disk full",
      message: { text: "Opening…", error: false },
      image: image({ phase: "error", error: "Unsupported file" }),
    }),
    { text: "Disk full", title: "Disk full", error: true },
  );
  assert.equal(
    status({ saving: true, tileError: "Detail failed." }).text,
    "Saving annotations…",
  );
});

test("transient messages outrank the image state until the next image update", () => {
  assert.deepEqual(
    status({ image: null, message: { text: "Opening…", error: false } }),
    { text: "Opening…", title: "Opening…", error: false },
  );
  assert.deepEqual(
    status({ message: { text: "Plane unavailable", error: true } }),
    { text: "Plane unavailable", title: "Plane unavailable", error: true },
  );
});

test("a completed save reports its warning, then Saved until the next edit", () => {
  assert.equal(
    status({ savedPath: "/a.tif", saveNotice: "Saved without labels." }).text,
    "Saved without labels.",
  );
  assert.equal(status({ savedPath: "/a.tif" }).text, "Saved");
  assert.equal(status({ savedPath: "/a.tif", dirty: true }).text, "");
  assert.equal(
    status({ savedPath: "/a.tif", image: image({ phase: "loading" }) }).text,
    "Loading detail…",
  );
});
