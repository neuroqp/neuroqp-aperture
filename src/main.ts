import OpenSeadragon from "openseadragon";
import { convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save, confirm } from "@tauri-apps/plugin-dialog";
import { AnnotationViewer } from "./annotation-viewer";
import {
  addPoint,
  movePoint,
  deletePoint,
  counterCount,
  settingsForRois,
  updatePointRois,
  selectCounter,
  type PointRoi,
  type AnnotationSettings,
} from "./annotations";
import appIcon from "../src-tauri/icons/icon.png";
import { icon, type IconName } from "./icons";
import { bindTools, toolPanels } from "./tools";
import { bindTheme } from "./theme";
import { applyDisplay, loadHistogram } from "./display";
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
      <button id="save-button" class="file-button" type="button" title="Save annotated TIFF (Ctrl+S)" disabled>${icon("save")}<span>Save</span></button>
    </div>
    <button id="theme-button" class="tool theme-button" type="button" aria-label="Switch theme"></button>
    <div class="window-actions">
      <button id="minimize-button" class="window-button" type="button" title="Minimize" aria-label="Minimize">${icon("minimize")}</button>
      <button id="maximize-button" class="window-button" type="button" title="Maximize" aria-label="Maximize">${icon("maximize")}</button>
      <button id="close-button" class="window-button close" type="button" title="Close" aria-label="Close">${icon("cancel")}</button>
    </div>
  </header>
  <nav class="quick-bar" aria-label="Image tools">
    <div class="tool-group" role="group" aria-label="Image adjustments and annotation tools">
      ${tool("brightness-button", "brightness", "Display range", true)}
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
  <dialog id="save-dialog" aria-labelledby="save-dialog-title" aria-describedby="save-dialog-description save-dialog-path">
    <form method="dialog">
      <h2 id="save-dialog-title">Save annotated TIFF</h2>
      <p id="save-dialog-description">Overwrite this TIFF or save to another file. Image pixels stay unchanged.</p>
      <p id="save-dialog-path" class="save-target"></p>
      <div class="save-actions">
        <button id="save-overwrite-button" class="file-button save-primary" type="submit" value="overwrite">Overwrite</button>
        <button id="save-as-button" class="file-button" type="submit" value="save-as">Save as...</button>
        <button id="save-cancel-button" class="file-button" type="submit" value="cancel" autofocus>Cancel</button>
      </div>
    </form>
  </dialog>
`;

const $ = <T extends HTMLElement = HTMLElement>(id: string) =>
  document.getElementById(id)! as T;
const tools = bindTools(applyDisplay, updatePointTool);
bindTheme();
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
  pixelsPerArrowPress: 300,
  gestureSettingsMouse: { clickToZoom: false, dblClickToZoom: true },
});

let currentImage: ImageState | null = null;
let rois: PointRoi[] = [];
let annotationsReady = false;
let annotationError: string | null = null;
let saveError: string | null = null;
let dirty = false;
let saving = false;
let savedPath: string | null = null;
const saveDialog = $<HTMLDialogElement>("save-dialog");
function editAnnotations(next: PointRoi[] | null) {
  if (!next || next === rois || !annotationsReady || saving) return;
  rois = next;
  dirty = true;
  annotationViewer.setRois(rois);
  updatePointTool();
}

const annotationViewer = new AnnotationViewer(viewer, {
  add: (point) => {
    if (currentImage)
      editAnnotations(
        addPoint(rois, point, tools.pointSettings(), currentImage),
      );
  },
  move: (reference, point) => {
    if (currentImage)
      editAnnotations(movePoint(rois, reference, point, currentImage));
  },
  delete: (reference) => editAnnotations(deletePoint(rois, reference)),
  active: () => tools.pointsActive(),
  editable: () => annotationsReady && !saving,
});

function updatePointTool(key?: keyof AnnotationSettings) {
  if (key === "counter" && annotationsReady && !saving) {
    rois = selectCounter(rois, tools.pointSettings().counter);
    annotationViewer.setRois(rois);
  } else if (
    key &&
    annotationsReady &&
    !saving &&
    key !== "allSlices" &&
    key !== "counter"
  ) {
    const settings = tools.pointSettings();
    const next = updatePointRois(rois, key, settings[key]);
    if (next !== rois) {
      rois = next;
      dirty = true;
      annotationViewer.setRois(rois);
    }
  }
  document
    .querySelectorAll<HTMLButtonElement | HTMLInputElement | HTMLSelectElement>(
      ".point-choice, #point-labels, #point-all-slices, #point-counter, #point-counter-options button[data-counter]",
    )
    .forEach((control) => {
      control.disabled = saving || !annotationsReady;
    });
  const active = tools.pointsActive();
  annotationViewer.updateInteraction();
  $("pan-button").classList.toggle("selected", !active);
  const count = counterCount(rois, tools.pointSettings().counter);
  $("point-count").textContent = String(count);
  $("point-count").title =
    `${rois.reduce((sum, roi) => sum + roi.points.length, 0)} points total`;
  $("filename").classList.toggle("unsaved", dirty);
  $<HTMLButtonElement>("save-button").disabled =
    !annotationsReady ||
    (!dirty && !rois.some((roi) => roi.points.length > 0)) ||
    saving;
}

async function discardChanges() {
  if (saving || dialogOpen) return false;
  if (!dirty) return true;
  dialogOpen = true;
  try {
    return await confirm(
      "Discard unsaved point annotations? Save first to keep them.",
      {
        title: "Unsaved annotations",
        kind: "warning",
        okLabel: "Discard",
        cancelLabel: "Cancel",
      },
    );
  } finally {
    dialogOpen = false;
  }
}

async function saveAnnotations() {
  if (
    !native ||
    saving ||
    dialogOpen ||
    !currentId ||
    $<HTMLButtonElement>("save-button").disabled
  )
    return;
  dialogOpen = true;
  saving = true;
  saveError = null;
  updatePointTool();
  if (currentImage) showImage(currentImage);
  let completed = false;
  try {
    const target = await invoke<{
      path: string;
      exists: boolean;
      isTiff: boolean;
    }>("annotation_target", {
      id: currentId,
      path: savedPath,
    });
    let path = target.path;
    let overwrite = false;
    if (target.isTiff || target.exists) {
      $("save-dialog-path").textContent = target.path;
      saveDialog.returnValue = "cancel";
      const choice = await new Promise<string>((resolve) => {
        saveDialog.addEventListener(
          "close",
          () => resolve(saveDialog.returnValue),
          { once: true },
        );
        saveDialog.showModal();
      });
      if (choice === "overwrite") overwrite = true;
      else if (choice === "save-as") {
        const selected = await save({
          title: "Save annotated TIFF as",
          defaultPath: target.path,
          filters: [{ name: "TIFF image", extensions: ["tif", "tiff"] }],
        });
        if (!selected) return;
        path = selected;
        // The native save picker confirms replacement of an existing file.
        overwrite = true;
      } else return;
    }
    if (!/\.tiff?$/i.test(path) && !(target.isTiff && path === target.path))
      throw new Error("Use a .tif or .tiff filename for the annotated TIFF.");
    $("status").classList.remove("error");
    $("status").textContent = "Saving annotations…";
    await invoke("save_annotations", { id: currentId, path, rois, overwrite });
    savedPath = path;
    dirty = false;
    completed = true;
  } catch (error) {
    saveError = String(error);
  } finally {
    saving = false;
    dialogOpen = false;
    updatePointTool();
    if (currentImage) showImage(currentImage);
    if (completed && !$("status").classList.contains("error")) {
      $("status").textContent = "Saved";
      $("status").title = "Saved";
    }
  }
}

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
  currentImage = null;
  savedPath = null;
  rois = [];
  tools.setPointSettings(settingsForRois([], 0));
  annotationsReady = false;
  annotationError = null;
  saveError = null;
  dirty = false;
  annotationViewer.setImage(null);
  annotationViewer.setRois([]);
  updatePointTool();
  previewAdded = false;
  pyramidAdded = false;
  tileError = null;
  viewer.close();
  tools.resetImage();
  document
    .querySelectorAll<HTMLButtonElement>("[data-image-control]")
    .forEach((button) => (button.disabled = true));
  $("dimensions").textContent = "";
}

function showError(message: string) {
  setLoading(false);
  $("viewer").dataset.phase = "error";
  $("status").textContent = saveError || message;
  $("status").title = saveError || message;
  $("status").classList.add("error");
}

function showImage(state: ImageState) {
  if (currentId !== state.id) {
    resetViewer();
    currentId = state.id;
    void invoke<PointRoi[]>("load_annotations", { id: state.id }).then(
      (loaded) => {
        if (currentId !== state.id) return;
        rois = loaded;
        tools.setPointSettings(
          settingsForRois(rois, tools.pointSettings().counter),
        );
        annotationsReady = true;
        annotationViewer.setRois(rois);
        updatePointTool();
      },
      (error) => {
        if (currentId !== state.id) return;
        annotationError = `Annotations could not load: ${String(error)}`;
        $("status").textContent = annotationError;
        $("status").classList.add("error");
      },
    );
  }
  currentImage = state;
  if (state.width > 0) annotationViewer.setImage(state);
  $("filename").textContent = state.name;
  $("filename").classList.toggle("unsaved", dirty);
  $("viewer").dataset.phase = state.phase;
  $("filename").title = state.name;
  setLoading(state.phase === "loading", state.progress);
  $("status").classList.toggle(
    "error",
    state.phase === "error" ||
      saveError !== null ||
      annotationError !== null ||
      (tileError !== null && state.phase !== "cancelled"),
  );
  const status =
    saveError ||
    (state.phase === "error"
      ? state.error || "Could not load image"
      : annotationError
        ? annotationError
        : saving
          ? "Saving annotations…"
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
                : "");
  if ($("status").textContent !== status) $("status").textContent = status;
  $("status").title = saveError || tileError || state.detail || status;
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
    void loadHistogram(`${tileUrl(state.id)}/preview.jpg`).then(
      (bins) => {
        if (currentId === state.id) tools.setHistogram(bins);
      },
      () => {
        if (currentId === state.id) tools.setHistogram(null);
      },
    );
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
        tools.setImageReady(true);
        document
          .querySelectorAll<HTMLButtonElement>("[data-image-control]")
          .forEach((button) => (button.disabled = false));
        updatePointTool();
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

async function openPath(path: string, checked = false) {
  if (!checked && !(await discardChanges())) return;
  resetViewer();
  $("viewer").dataset.phase = "loading";
  $("filename").textContent = path.split(/[\\/]/).pop() || path;
  $("status").textContent = "Opening…";
  $("status").classList.remove("error");
  setLoading(true);
  await loader.open(path);
}

async function chooseImage() {
  if (!native || dialogOpen || !(await discardChanges())) return;
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
    if (typeof path === "string") await openPath(path, true);
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

$("save-button").addEventListener("click", () => void saveAnnotations());
$("open-button").addEventListener("click", () => void chooseImage());
$("cancel-button").addEventListener("click", () => void cancelLoad());
$("fit-button").addEventListener("click", () => viewer.viewport.goHome());
$("pan-button").addEventListener("click", () => {
  tools.deactivatePoints();
  viewer.canvas.focus();
});
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
  if (saveError) return;
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
  if (dialogOpen || saving) return;
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "o") {
    event.preventDefault();
    void chooseImage();
  } else if (
    (event.ctrlKey || event.metaKey) &&
    event.key.toLowerCase() === "s"
  ) {
    event.preventDefault();
    void saveAnnotations();
  } else if (event.key === "Escape") {
    if (tools.close()) event.preventDefault();
    else void cancelLoad();
  } else if (
    !(
      event.target instanceof Element &&
      event.target.closest(
        "input, select, textarea, [contenteditable], .tool-panel",
      )
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
  void windowHandle
    .onCloseRequested(async (event) => {
      if (dirty || saving || dialogOpen) {
        event.preventDefault();
        if (await discardChanges()) {
          dirty = false;
          await windowHandle.close();
        }
      }
    })
    .catch(showError);
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
  annotationViewer.dispose();
  viewer.destroy();
});
