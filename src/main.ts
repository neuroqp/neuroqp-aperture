import OpenSeadragon from "openseadragon";
import { convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import appIcon from "../src-tauri/icons/icon.png";
import { icon, type IconName } from "./icons";
import { bindTools, toolPanels } from "./tools";
import {
  ImageLoader,
  orientation,
  preview,
  pyramid,
  tileFailure,
  type ImageState,
} from "./image";
import "./style.css";

const tool = (id: string, name: IconName, label: string, panel = false) =>
  `<button id="${id}" class="tool" type="button" title="${label}" aria-label="${label}" ${panel ? `aria-pressed="false" aria-expanded="false" aria-controls="${name}-panel"` : "data-image-control disabled"}>${icon(name)}</button>`;

document.querySelector<HTMLDivElement>("#app")!.innerHTML = `
  <header class="image-bar">
    <div class="image-title" data-tauri-drag-region>
      <img src="${appIcon}" width="20" height="20" alt="" draggable="false" data-tauri-drag-region>
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
    <div class="tool-group" role="group" aria-label="Image adjustments and annotation tools">
      ${tool("brightness-button", "brightness", "Brightness & contrast", true)}
      ${tool("points-button", "points", "Multi-point", true)}
    </div>
    <div class="load-status">
      <span id="status" role="status" aria-live="polite"></span>
      <button id="cancel-button" class="tool" type="button" title="Cancel loading (Escape)" aria-label="Cancel loading" hidden>${icon("cancel")}</button>
      <span id="dimensions"></span>
    </div>
    <div class="tool-group viewer-controls" role="group" aria-label="Viewer controls">
      ${tool("pan-button", "hand", "Pan image")}
      ${tool("zoom-in-button", "zoomIn", "Zoom in (+)")}
      ${tool("zoom-out-button", "zoomOut", "Zoom out (-)")}
      ${tool("fit-button", "fit", "Fit image (F)")}
    </div>
    <div id="progress" role="progressbar" aria-label="Image loading" hidden><span></span></div>
  </nav>
  <div class="viewer-workspace">
    <section id="viewer" aria-label="Image viewer"></section>
    ${toolPanels}
  </div>
`;

const $ = <T extends HTMLElement = HTMLElement>(id: string) =>
  document.getElementById(id)! as T;
const tools = bindTools();
const native = isTauri();
const windowHandle = native ? getCurrentWindow() : null;
const viewer = OpenSeadragon({
  element: $("viewer"),
  showNavigationControl: false,
  showSequenceControl: false,
  showNavigator: false,
  drawer: "canvas",
  imageSmoothingEnabled: false,
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
  } else if (event.key === "Escape") {
    if (tools.close()) event.preventDefault();
    else void cancelLoad();
  } else if (
    !(
      event.target instanceof Element &&
      event.target.closest("input, select, textarea, [contenteditable]")
    ) &&
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
