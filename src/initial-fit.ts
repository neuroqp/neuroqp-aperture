import type OpenSeadragon from "openseadragon";

/** Fit once after OSD has finished resizing to the first usable layout. */
export function fitInitialImage(viewer: OpenSeadragon.Viewer) {
  const cancel = () => viewer.removeHandler("update-viewport", fit);
  const fit = () => {
    const { clientWidth: width, clientHeight: height } = viewer.container;
    if (width <= 1 || height <= 1) return;
    const size = viewer.viewport.getContainerSize();
    if (size.x !== width || size.y !== height) {
      viewer.forceResize();
      return;
    }
    cancel();
    viewer.viewport.goHome(true);
  };
  // This event follows resize bookkeeping and its final zoom adjustment.
  viewer.addHandler("update-viewport", fit);
  viewer.forceRedraw();
  return cancel;
}
