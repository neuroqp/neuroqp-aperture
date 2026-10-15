import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import { runInNewContext } from "node:vm";
import type OpenSeadragon from "openseadragon";
import { fitInitialImage } from "./initial-fit.ts";

// Load the installed OSD runtime, including real viewport geometry, without a GUI.
const element = () => ({
  style: {},
  addEventListener() {},
  removeEventListener() {},
  getContext: () => ({}),
});
const context = {
  module: { exports: {} },
  self: {},
  navigator: { appName: "", appVersion: "", userAgent: "" },
  document: {
    documentElement: element(),
    createElement: element,
    addEventListener() {},
    removeEventListener() {},
  },
  window: {
    console,
    location: { search: "" },
    setTimeout,
    clearTimeout,
    addEventListener() {},
    removeEventListener() {},
  },
};
const source = readFileSync(
  createRequire(import.meta.url).resolve("openseadragon"),
  "utf8",
);
const resizeDeclaration = "function doViewerResize(viewer, containerSize) {";
assert.ok(source.includes(resizeDeclaration));
// Expose the real private resize routine, without copying its zoom algorithm.
runInNewContext(
  source.replace(
    resizeDeclaration,
    `$.resizeForTest = function(viewer, size) {
      THIS[viewer.hash] = { prevContainerSize: viewer.viewport.getContainerSize() };
      doViewerResize(viewer, size);
    };
    ${resizeDeclaration}`,
  ),
  context,
);
const osd = context.module.exports as typeof OpenSeadragon & {
  resizeForTest(viewer: OpenSeadragon.Viewer, size: OpenSeadragon.Point): void;
};

function setup() {
  const events = new osd.EventSource();
  const container = { clientWidth: 1280, clientHeight: 0 };
  const viewport = new osd.Viewport({
    containerSize: new osd.Point(1280, 1),
    degrees: 0,
  });
  viewport.resetContentSize(new osd.Point(65, 49));
  viewport.goHome(true);
  let resizeRequested = false;
  const viewer = Object.assign(events, {
    hash: "initial-fit-test",
    preserveImageSizeOnResize: false,
    container,
    viewport,
    forceResize: () => {
      resizeRequested = true;
    },
    forceRedraw() {},
  }) as unknown as OpenSeadragon.Viewer;
  viewport.viewer = viewer;
  return {
    viewer,
    container,
    viewport,
    frame() {
      if (resizeRequested) {
        resizeRequested = false;
        osd.resizeForTest(
          viewer,
          new osd.Point(container.clientWidth, container.clientHeight),
        );
      }
      const zoomBeforeFit = viewport.getZoom(true);
      events.raiseEvent("update-viewport");
      return zoomBeforeFit;
    },
  };
}

test("initial zero-height fit waits for layout and OSD resize, then fits only once", () => {
  const { viewer, container, viewport, frame } = setup();
  const tinyZoom = viewport.getZoom(true);
  assert.ok(1280 * tinyZoom < 2, "reproduces the tiny initial image");
  fitInitialImage(viewer);
  frame();
  assert.equal(viewport.getZoom(true), tinyZoom);

  container.clientHeight = 697;
  frame();
  assert.equal(viewport.getZoom(true), tinyZoom, "resize is asynchronous");
  const zoomAfterOsdResize = frame();
  assert.ok(
    1280 * zoomAfterOsdResize < 2,
    "OSD resize alone retains tiny zoom",
  );
  assert.ok(Math.abs(viewport.getZoom(true) - viewport.getHomeZoom()) < 1e-12);
  assert.ok(1280 * viewport.getZoom(true) > 900);

  viewport.zoomBy(2, undefined, true);
  const userZoom = viewport.getZoom(true);
  frame();
  assert.equal(viewport.getZoom(true), userZoom);
  container.clientWidth = 1000;
  osd.resizeForTest(viewer, new osd.Point(1000, 697));
  const resizedZoom = viewport.getZoom(true);
  frame();
  assert.equal(
    viewport.getZoom(true),
    resizedZoom,
    "later resizes do not refit",
  );
});

test("replacing an image cancels its pending fit", () => {
  const { viewer, container, viewport, frame } = setup();
  const cancel = fitInitialImage(viewer);
  cancel();
  container.clientHeight = 697;
  osd.resizeForTest(viewer, new osd.Point(1280, 697));
  viewport.resetContentSize(new osd.Point(100, 100));
  viewport.zoomTo(0.25, undefined, true);
  frame();
  assert.equal(viewport.getZoom(true), 0.25);

  fitInitialImage(viewer);
  frame();
  assert.ok(Math.abs(viewport.getZoom(true) - viewport.getHomeZoom()) < 1e-12);
});
