import OpenSeadragon from "openseadragon";
import { convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import {
  ImageLoader,
  orientation,
  preview,
  pyramid,
  tileFailure,
  type ImageState,
} from "./image";
import "./style.css";

const icons: Record<string, string> = {
  image:
    '<rect x="3" y="3" width="14" height="14" rx="2"/><path d="m3 13 4-4 4 4 2-2 4 4"/><circle cx="13" cy="7" r="1"/>',
  open: '<path d="M3 7V4h5l2 2h7v3M3 7h15l-3 9H2Z"/>',
  save: '<path d="M4 2h10l3 3v13H3V2Z"/><path d="M6 2v5h7V2M6 18v-7h8v7"/>',
  rectangle:
    '<rect x="3" y="4" width="14" height="12" stroke-dasharray="2 2"/>',
  oval: '<ellipse cx="10" cy="10" rx="7" ry="6" stroke-dasharray="2 2"/>',
  polygon: '<path d="m4 3 12 2 1 9-8 3-6-7Z" stroke-dasharray="2 2"/>',
  line: '<path d="m3 17 14-14"/><path d="M3 14v3h3M14 3h3v3"/>',
  point: '<path d="M10 2v16M2 10h16"/><circle cx="10" cy="10" r="3"/>',
  wand: '<path d="m3 17 10-10M11 9l2 2M13 2v2M18 7h-2M17 3l-2 2M8 3l2 2"/>',
  text: '<path d="M3 5V3h14v2M10 3v14M7 17h6"/>',
  hand: '<path d="M5 10V6a1.5 1.5 0 0 1 3 0V3a1.5 1.5 0 0 1 3 0v3a1.5 1.5 0 0 1 3 0v1a1.5 1.5 0 0 1 3 0v5c0 4-2 6-5 6H9c-2 0-3-1-4-3l-3-4a1.5 1.5 0 0 1 2-2Z"/>',
  zoomIn: '<circle cx="8" cy="8" r="5.5"/><path d="m12 12 5 5M5 8h6M8 5v6"/>',
  zoomOut: '<circle cx="8" cy="8" r="5.5"/><path d="m12 12 5 5M5 8h6"/>',
  fit: '<path d="M7 3H3v4M13 3h4v4M3 13v4h4M17 13v4h-4"/><rect x="6" y="6" width="8" height="8"/>',
  cancel: '<path d="m5 5 10 10M15 5 5 15"/>',
  minimize: '<path d="M4 13h12"/>',
  maximize: '<rect x="4" y="4" width="12" height="12"/>',
  restore:
    '<path d="M7 5V3h10v10h-2"/><rect x="3" y="7" width="10" height="10"/>',
};
const icon = (name: string) =>
  `<svg viewBox="0 0 20 20" aria-hidden="true">${icons[name]}</svg>`;
const tool = (id: string, name: string, label: string, placeholder = false) =>
  `<button id="${id}" class="tool" type="button" title="${label}${placeholder ? ": not available yet" : ""}" aria-label="${label}" ${placeholder ? "disabled" : "data-image-control disabled"}>${icon(name)}</button>`;

document.querySelector<HTMLDivElement>("#app")!.innerHTML = `
  <header class="image-bar">
    <div class="image-title" data-tauri-drag-region>
      ${icon("image")}
      <span id="filename" data-tauri-drag-region>NeuroQP Aperture</span>
    </div>
    <div class="file-actions">
      <button id="open-button" class="file-button" type="button" title="Open image (Ctrl+O)">${icon("open")}<span>Open</span></button>
      <button id="save-button" class="file-button" type="button" title="Saving is not available in this preview" disabled>${icon("save")}<span>Save</span></button>
    </div>
    <div class="window-actions">
      <button id="minimize-button" class="window-button" type="button" title="Minimize" aria-label="Minimize">${icon("minimize")}</button>
      <button id="maximize-button" class="window-button" type="button" title="Maximize" aria-label="Maximize">${icon("maximize")}</button>
      <button id="close-button" class="window-button close" type="button" title="Close" aria-label="Close">${icon("cancel")}</button>
    </div>
  </header>
  <nav class="quick-bar" aria-label="Image tools">
    <div class="tool-group">
      ${tool("rectangle-tool", "rectangle", "Rectangle selection", true)}
      ${tool("oval-tool", "oval", "Oval selection", true)}
      ${tool("polygon-tool", "polygon", "Polygon selection", true)}
      ${tool("line-tool", "line", "Line", true)}
      ${tool("point-tool", "point", "Point", true)}
      ${tool("wand-tool", "wand", "Wand selection", true)}
      ${tool("text-tool", "text", "Text", true)}
    </div>
    <span class="separator" aria-hidden="true"></span>
    <div class="tool-group">
      ${tool("pan-button", "hand", "Pan image")}
      ${tool("zoom-in-button", "zoomIn", "Zoom in (+)")}
      ${tool("zoom-out-button", "zoomOut", "Zoom out (-)")}
      ${tool("fit-button", "fit", "Fit image (F)")}
    </div>
    <div class="load-status">
      <span id="status" role="status" aria-live="polite"></span>
      <button id="cancel-button" class="tool" type="button" title="Cancel loading (Escape)" aria-label="Cancel loading" hidden>${icon("cancel")}</button>
      <span id="dimensions"></span>
    </div>
    <div id="progress" role="progressbar" aria-label="Image loading" hidden><span></span></div>
  </nav>
  <section id="viewer" aria-label="Image viewer"></section>
`;

const $ = <T extends HTMLElement = HTMLElement>(id: string) =>
  document.getElementById(id)! as T;
const native = isTauri();
const windowHandle = native ? getCurrentWindow() : null;
const viewer = OpenSeadragon({
  element: $("viewer"),
  showNavigationControl: false,
  showSequenceControl: false,
  showNavigator: false,
  drawer: "canvas",
  imageLoaderLimit: 4,
  maxImageCacheCount: 128,
  timeout: 600_000,
  blendTime: 0.12,
  animationTime: 0.25,
  visibilityRatio: 0.2,
  constrainDuringPan: false,
  maxZoomPixelRatio: 4,
  gestureSettingsMouse: { clickToZoom: false, dblClickToZoom: true },
});

let currentId: string | null = null;
let previewAdded = false;
let pyramidAdded = false;
let dialogOpen = false;
let loading = false;
let tileError: string | null = null;
const tileUrl = (id: string) => convertFileSrc(id, "aperture");

function setLoading(active: boolean, progress: number | null = null) {
  loading = active;
  $("cancel-button").hidden = !active;
  const bar = $("progress");
  bar.hidden = !active;
  bar.classList.toggle("indeterminate", progress === null);
  if (progress === null) bar.removeAttribute("aria-valuenow");
  else {
    const value = Math.min(100, Math.max(0, progress * 100));
    bar.setAttribute("aria-valuenow", String(Math.round(value)));
    bar.firstElementChild!.setAttribute(
      "style",
      `transform:scaleX(${value / 100})`,
    );
  }
}

function resetViewer() {
  currentId = null;
  previewAdded = false;
  pyramidAdded = false;
  tileError = null;
  viewer.close();
  document
    .querySelectorAll<HTMLButtonElement>("[data-image-control]")
    .forEach((button) => (button.disabled = true));
  $("dimensions").textContent = "";
}

function showError(message: string) {
  setLoading(false);
  $("viewer").dataset.phase = "error";
  $("status").textContent = message;
  $("status").title = message;
  $("status").classList.add("error");
}

function showImage(state: ImageState) {
  if (currentId !== state.id) {
    resetViewer();
    currentId = state.id;
  }
  $("filename").textContent = state.name;
  $("viewer").dataset.phase = state.phase;
  $("filename").title = state.name;
  setLoading(state.phase === "loading", state.progress);
  $("status").classList.toggle(
    "error",
    state.phase === "error" ||
      (tileError !== null && state.phase !== "cancelled"),
  );
  const status =
    state.phase === "error"
      ? state.error || "Could not load image"
      : state.phase === "cancelled"
        ? "Cancelled"
        : tileError
          ? tileError
          : state.phase === "loading"
            ? state.previewReady
              ? "Loading detail…"
              : state.width > 0
                ? "Building preview…"
                : "Reading image…"
            : "";
  if ($("status").textContent !== status) $("status").textContent = status;
  $("status").title = tileError || state.detail || status;
  const transform = orientation(state.orientation);
  const [displayWidth, displayHeight] =
    transform.degrees % 180 === 0
      ? [state.width, state.height]
      : [state.height, state.width];
  if (state.width > 0 && state.height > 0)
    $("dimensions").textContent =
      `${displayWidth.toLocaleString()} × ${displayHeight.toLocaleString()}`;

  if (state.previewReady && !previewAdded) {
    previewAdded = true;
    viewer.addTiledImage({
      tileSource: preview(state, tileUrl),
      ...transform,
      width: 1,
      index: 0,
      success: (event) => {
        if (currentId !== state.id) {
          viewer.world.removeItem(
            (event as unknown as { item: OpenSeadragon.TiledImage }).item,
          );
          return;
        }
        viewer.viewport.goHome(true);
        document
          .querySelectorAll<HTMLButtonElement>("[data-image-control]")
          .forEach((button) => (button.disabled = false));
        $("pan-button").classList.add("selected");
        if (state.tilesReady) addPyramid(state);
      },
      error: () => {
        if (currentId === state.id)
          showError("Could not display the image preview.");
      },
    });
  }
  if (state.tilesReady && previewAdded && viewer.world.getItemCount() > 0)
    addPyramid(state);
}

function addPyramid(state: ImageState) {
  if (pyramidAdded || state.width <= 0 || state.height <= 0) return;
  pyramidAdded = true;
  viewer.addTiledImage({
    tileSource: pyramid(state, tileUrl),
    ...orientation(state.orientation),
    width: 1,
    index: 1,
    success: (event) => {
      if (currentId !== state.id)
        viewer.world.removeItem(
          (event as unknown as { item: OpenSeadragon.TiledImage }).item,
        );
    },
    error: () => {
      if (currentId === state.id)
        showError("Detail could not load. Preview remains available.");
    },
  });
}

const loader = new ImageLoader(
  {
    open: (path) => invoke<ImageState>("open_image", { path }),
    state: () => invoke<ImageState | null>("image_state"),
    cancel: () => invoke("cancel_load"),
  },
  showImage,
  showError,
);

async function openPath(path: string) {
  resetViewer();
  $("viewer").dataset.phase = "loading";
  $("filename").textContent = path.split(/[\\/]/).pop() || path;
  $("status").textContent = "Opening…";
  $("status").classList.remove("error");
  setLoading(true);
  await loader.open(path);
}

async function chooseImage() {
  if (!native || dialogOpen) return;
  dialogOpen = true;
  try {
    const path = await open({
      multiple: false,
      directory: false,
      filters: [
        {
          name: "Images",
          extensions: ["jpg", "jpeg", "png", "bmp", "tif", "tiff", "webp"],
        },
      ],
    });
    if (typeof path === "string") await openPath(path);
  } catch (error) {
    showError(String(error));
  } finally {
    dialogOpen = false;
  }
}

async function cancelLoad() {
  if (!loading) return;
  $("status").textContent = "Cancelling…";
  $<HTMLButtonElement>("cancel-button").disabled = true;
  if (await loader.cancel()) {
    setLoading(false);
    $("status").textContent = "Cancelled";
    $("viewer").dataset.phase = "cancelled";
  }
  $<HTMLButtonElement>("cancel-button").disabled = false;
}

$("open-button").addEventListener("click", () => void chooseImage());
$("cancel-button").addEventListener("click", () => void cancelLoad());
$("fit-button").addEventListener("click", () => viewer.viewport.goHome());
$("pan-button").addEventListener("click", () => viewer.canvas.focus());
viewer.addHandler("tile-load-failed", (event) => {
  const message = tileFailure(
    currentId ? tileUrl(currentId) : null,
    $("viewer").dataset.phase,
    event.tile.url,
  );
  if (
    !message ||
    tileError === message ||
    tileError === "Could not display the image preview."
  )
    return;
  tileError = message;
  $("status").textContent = message;
  $("status").title = message;
  $("status").classList.add("error");
});
$("zoom-in-button").addEventListener("click", () => {
  viewer.viewport.zoomBy(1.5);
  viewer.viewport.applyConstraints();
});
$("zoom-out-button").addEventListener("click", () => {
  viewer.viewport.zoomBy(1 / 1.5);
  viewer.viewport.applyConstraints();
});
document.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "o") {
    event.preventDefault();
    void chooseImage();
  } else if (event.key === "Escape") void cancelLoad();
  else if (
    !event.ctrlKey &&
    !event.metaKey &&
    !event.altKey &&
    !$<HTMLButtonElement>("fit-button").disabled
  ) {
    if (event.key.toLowerCase() === "f") viewer.viewport.goHome();
    if (event.key === "+" || event.key === "=") {
      event.preventDefault();
      $("zoom-in-button").click();
    }
    if (event.key === "-") {
      event.preventDefault();
      $("zoom-out-button").click();
    }
  }
});

if (windowHandle) {
  $("minimize-button").addEventListener(
    "click",
    () => void windowHandle.minimize().catch(showError),
  );
  $("maximize-button").addEventListener(
    "click",
    () => void windowHandle.toggleMaximize().catch(showError),
  );
  $("close-button").addEventListener(
    "click",
    () => void windowHandle.close().catch(showError),
  );
  const syncMaximize = async () => {
    const maximized = await windowHandle.isMaximized();
    const button = $("maximize-button");
    button.innerHTML = icon(maximized ? "restore" : "maximize");
    button.title = maximized ? "Restore" : "Maximize";
    button.setAttribute("aria-label", button.title);
  };
  void syncMaximize().catch(showError);
  void windowHandle.onResized(() => void syncMaximize().catch(showError));
  void windowHandle
    .onDragDropEvent((event) => {
      $("viewer").classList.toggle(
        "drop-target",
        event.payload.type === "over",
      );
      if (event.payload.type === "drop" && event.payload.paths[0])
        void openPath(event.payload.paths[0]);
    })
    .catch(showError);
  void loader.restore();
} else {
  $<HTMLButtonElement>("open-button").disabled = true;
  $("open-button").title = "Open images in the desktop app";
  document
    .querySelectorAll<HTMLButtonElement>(".window-button")
    .forEach((button) => (button.disabled = true));
}

window.addEventListener("beforeunload", () => {
  loader.dispose();
  viewer.destroy();
});
