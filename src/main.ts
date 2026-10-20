import OpenSeadragon from "openseadragon";
import { convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save, confirm } from "@tauri-apps/plugin-dialog";
import { AnnotationViewer } from "./annotation-viewer";
import { FolderNavigation } from "./folder-navigation";
import {
  saveAnnotationFile,
  type AnnotationTarget,
  type SaveResult,
} from "./save-flow";
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
import neuroqpLogo from "./assets/neuroqp-logo.png";
import { icon, type IconName } from "./icons";
import { bindTools, toolPanels } from "./tools";
import { bindTheme } from "./theme";
import { bindSettings, settingsPanel } from "./settings";
import { bindUpdates, updatePanel } from "./updates";
import { loadChannelHistograms } from "./display";
import { channelHistograms } from "./display-settings";
import { bindColorPanel } from "./color-panel";
import { bindDimensionNavigation } from "./dimension-navigation";
import { visibleChannelSettings } from "./color-settings";
import { bindColorRenderer } from "./color-renderer";
import { createGpuColorDrawer } from "./gpu-color-drawer";
import { fitInitialImage } from "./initial-fit";
import {
  ImageLoader,
  imageTileCacheCount,
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
      <div class="app-brand" data-tauri-drag-region>
        <img class="brand-logo" src="${neuroqpLogo}" width="98" height="24" alt="NeuroQP" draggable="false" data-tauri-drag-region>
        <span data-tauri-drag-region>Aperture</span>
      </div>
      <button id="previous-image-button" class="tool image-navigation" type="button" title="Previous image" aria-label="Previous image" disabled>${icon("previous")}</button>
      <div class="filename-progress" data-tauri-drag-region>
        <div id="progress" role="progressbar" aria-label="Image loading" aria-valuemin="0" aria-valuemax="100" hidden><span></span></div>
        <span id="filename" data-tauri-drag-region></span>
        <span id="loading-percent" aria-hidden="true" data-tauri-drag-region hidden></span>
      </div>
      <button id="next-image-button" class="tool image-navigation" type="button" title="Next image" aria-label="Next image" disabled>${icon("next")}</button>
    </div>
    <div class="file-actions">
      <button id="open-button" class="file-button" type="button" title="Open image (Ctrl+O)">${icon("open")}<span>Open</span></button>
      <button id="save-button" class="file-button" type="button" title="Save annotated TIFF (Ctrl+S)" disabled>${icon("save")}<span>Save</span></button>
    </div>
    <button id="settings-button" class="tool" type="button" title="Settings" aria-label="Settings" aria-haspopup="dialog" aria-controls="settings-dialog">${icon("settings")}</button>
    <div class="window-actions">
      <button id="minimize-button" class="window-button" type="button" title="Minimize" aria-label="Minimize">${icon("minimize")}</button>
      <button id="maximize-button" class="window-button" type="button" title="Maximize" aria-label="Maximize">${icon("maximize")}</button>
      <button id="close-button" class="window-button close" type="button" title="Close" aria-label="Close">${icon("cancel")}</button>
    </div>
  </header>
  <nav class="quick-bar" aria-label="Image tools">
    <div class="tool-group" role="group" aria-label="Image adjustments and annotation tools">
      ${tool("color-button", "color", "Color", true)}
      ${tool("navigator-button", "navigator", "Dimensions", true)}
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
  </nav>
  <div class="viewer-workspace">
    <section id="viewer" aria-label="Image viewer"></section>
    ${toolPanels}
  </div>
  ${settingsPanel}
  ${updatePanel}
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
const tools = bindTools(updatePointTool, {
  visibility: (open) => {
    if (open) colorPanel.redraw();
    else colorPanel.closeTransient();
  },
  handleEscape: () => colorPanel.handleEscape(),
});
const settings = bindSettings();
bindTheme();
const native = isTauri();
const windowHandle = native ? getCurrentWindow() : null;
let colorRenderer: ReturnType<typeof bindColorRenderer> | undefined;
const colorPanel = bindColorPanel(() => colorRenderer?.refresh());
const dimensionNavigation = bindDimensionNavigation(
  async (position, z, time, hint, projection) => {
    if (
      !native ||
      saving ||
      dialogOpen ||
      $("settings-dialog").hasAttribute("open")
    )
      return false;
    setLoading(true);
    return loader.selectPlane(position, z, time, hint, projection);
  },
  () => colorRenderer?.refresh(),
);
const displayColors = () =>
  visibleChannelSettings(colorPanel.settings(), dimensionNavigation.channel());
const colorError = (message: string) => {
  tileError = message;
  if (currentImage) showImage(currentImage);
};
const GpuColorDrawer = createGpuColorDrawer(
  OpenSeadragon,
  displayColors,
  (message) => queueMicrotask(() => colorError(message)),
);
const viewer = OpenSeadragon({
  element: $("viewer"),
  showNavigationControl: false,
  showSequenceControl: false,
  showNavigator: false,
  // A diagnostic switch keeps the previous renderer available for native A/B checks.
  drawer:
    new URLSearchParams(location.search).get("colorRenderer") === "cpu"
      ? "canvas"
      : [GpuColorDrawer, "canvas"],
  crossOriginPolicy: "Anonymous",
  imageSmoothingEnabled: false,
  imageLoaderLimit: 4,
  maxImageCacheCount: imageTileCacheCount(),
  timeout: 600_000,
  blendTime: 0.12,
  animationTime: 0.25,
  visibilityRatio: 0.2,
  constrainDuringPan: false,
  maxZoomPixelRatio: 4,
  pixelsPerArrowPress: 300,
  gestureSettingsMouse: { clickToZoom: false, dblClickToZoom: true },
});
if (new URLSearchParams(location.search).has("colorBenchmark")) {
  Object.assign(window, { __apertureViewer: viewer });
}

colorRenderer = bindColorRenderer(
  viewer,
  displayColors,
  colorError,
  (imageId, data) => {
    if (currentId === imageId)
      colorPanel.setHistograms(
        channelHistograms(data.planes, data.width, data.height),
      );
  },
);

let currentImage: ImageState | null = null;
let rois: PointRoi[] = [];
let annotationsReady = false;
let annotationError: string | null = null;
let saveError: string | null = null;
let saveNotice: string | null = null;
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
  if (saving || dialogOpen || $<HTMLDialogElement>("settings-dialog").open)
    return false;
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

async function prepareUpdate() {
  if (saving || dialogOpen) return false;
  if (dirty) {
    dialogOpen = true;
    const dialog = $<HTMLDialogElement>("update-unsaved-dialog");
    dialog.returnValue = "cancel";
    const choice = await new Promise<string>((resolve) => {
      dialog.addEventListener("close", () => resolve(dialog.returnValue), {
        once: true,
      });
      dialog.showModal();
    });
    dialogOpen = false;
    if (choice === "save") {
      await saveAnnotations();
      if (dirty) return false;
    } else if (choice !== "discard") return false;
    // Keep discarded edits marked unsaved until exit; installation can still fail.
  }
  // Freeze edits before stopping image requests and joining native workers.
  saving = true;
  dialogOpen = true;
  histogramLoad?.abort();
  for (const job of downloads) job.abort();
  downloads.clear();
  updatePointTool();
  return true;
}

async function saveAnnotations() {
  if (
    !native ||
    saving ||
    dialogOpen ||
    $<HTMLDialogElement>("settings-dialog").open ||
    !currentId ||
    $<HTMLButtonElement>("save-button").disabled
  )
    return;
  dialogOpen = true;
  saving = true;
  saveError = null;
  saveNotice = null;
  updatePointTool();
  if (currentImage) showImage(currentImage);
  let completed = false;
  try {
    const target = await invoke<AnnotationTarget>("annotation_target", {
      id: currentId,
      path: savedPath,
    });
    const path = await saveAnnotationFile(target, {
      choose: async (target) => {
        $("save-dialog-path").textContent = target.path;
        $("save-dialog-title").textContent = target.warning
          ? "Review before overwriting"
          : "Save annotated TIFF";
        $("save-dialog-description").textContent = target.warning
          ? `${target.warning} Annotations from other windows are not merged.`
          : "Overwrite this TIFF or save to another file. Image pixels stay unchanged.";
        $("save-overwrite-button").textContent = target.warning
          ? "Overwrite anyway"
          : "Overwrite";
        saveDialog.returnValue = "cancel";
        return new Promise<string>((resolve) => {
          saveDialog.addEventListener(
            "close",
            () => resolve(saveDialog.returnValue),
            { once: true },
          );
          saveDialog.showModal();
        });
      },
      pick: (defaultPath) =>
        save({
          title: "Save annotated TIFF as",
          defaultPath,
          filters: [{ name: "TIFF image", extensions: ["tif", "tiff"] }],
        }),
      inspect: (path) =>
        invoke<AnnotationTarget>("annotation_target", { id: currentId, path }),
      write: async (path, overwrite, acknowledgment) => {
        $("status").classList.remove("error");
        $("status").textContent = "Saving annotations…";
        const result = await invoke<SaveResult>("save_annotations", {
          id: currentId,
          path,
          rois,
          overwrite,
          acknowledgment,
        });
        if (result.saved) saveNotice = result.warning;
        return result;
      },
    });
    if (!path) return;
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
      $("status").textContent = saveNotice || "Saved";
      $("status").title = saveNotice || "Saved";
    }
  }
}

let currentId: string | null = null;
let previewAdded = false;
let previewDisplayed = false;
let pendingPreview: OpenSeadragon.TiledImage | null = null;
const loadedImages = new WeakSet<OpenSeadragon.TiledImage>();
viewer.addHandler("tile-loaded", (event) => {
  loadedImages.add(event.tiledImage);
});
let pyramidAdded = false;
let dialogOpen = false;
let loading = false;
let tileError: string | null = null;
let histogramLoad: AbortController | null = null;
let cancelInitialFit: (() => void) | undefined;
let pendingViewportBounds: OpenSeadragon.Rect | null = null;
const downloads = new Set<OpenSeadragon.ImageJob>();
const tileUrl = (id: string) => convertFileSrc(id, "aperture");

function trackDownloads(
  source: OpenSeadragon.TileSourceOptions & {
    downloadTileStart?: (job: OpenSeadragon.ImageJob) => void;
  },
) {
  const start =
    source.downloadTileStart ||
    OpenSeadragon.TileSource.prototype.downloadTileStart;
  return {
    ...source,
    downloadTileStart(job: OpenSeadragon.ImageJob) {
      downloads.add(job);
      const finish = job.finish;
      job.finish = (...args) => {
        downloads.delete(job);
        finish.apply(job, args);
      };
      const fail = job.fail;
      job.fail = (...args) => {
        downloads.delete(job);
        fail.apply(job, args);
      };
      start.call(this, job);
    },
  };
}

function setLoading(active: boolean, progress: number | null = null) {
  loading = active;
  $("cancel-button").hidden = !active;
  const bar = $("progress");
  const percent = $("loading-percent");
  const value =
    active && progress !== null
      ? Math.round(Math.min(100, Math.max(0, progress * 100)))
      : null;
  bar.hidden = !active;
  bar.classList.toggle("indeterminate", active && value === null);
  percent.hidden = value === null;
  percent.textContent = value === null ? "" : `${value}%`;
  if (value === null) bar.removeAttribute("aria-valuenow");
  else bar.setAttribute("aria-valuenow", String(value));
  bar.firstElementChild!.setAttribute(
    "style",
    `transform:scaleX(${(value ?? 0) / 100})`,
  );
}

function resetViewer(preserveDisplay = false) {
  cancelInitialFit?.();
  cancelInitialFit = undefined;
  histogramLoad?.abort();
  histogramLoad = null;
  currentId = null;
  currentImage = null;
  savedPath = null;
  rois = [];
  tools.setPointSettings(settingsForRois([], 0));
  annotationsReady = false;
  annotationError = null;
  saveError = null;
  saveNotice = null;
  dirty = false;
  annotationViewer.setImage(null);
  annotationViewer.setRois([]);
  updatePointTool();
  previewAdded = false;
  previewDisplayed = false;
  if (pendingPreview) viewer.world.removeItem(pendingPreview);
  pendingPreview = null;
  pyramidAdded = false;
  tileError = null;
  if (!preserveDisplay) viewer.close();
  for (const job of downloads) job.abort();
  downloads.clear();
  if (!preserveDisplay) {
    pendingViewportBounds = null;
    colorPanel.setChannels([]);
    dimensionNavigation.setImage(null);
  }
  colorPanel.setEnabled(false);
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
  if (!selectedPath && state.path) {
    selectedPath = state.path;
    folder.select(state.path);
  }
  const previousImage = currentImage;
  const sameDimensions = Boolean(
    state.dimensions &&
    previousImage?.dimensions &&
    state.path &&
    state.path === previousImage.path,
  );
  const preservedBounds =
    currentId !== state.id && sameDimensions
      ? viewer.world.getItemCount() > 0
        ? viewer.viewport.getBounds()
        : pendingViewportBounds
      : null;
  if (currentId !== state.id) {
    resetViewer(sameDimensions);
    pendingViewportBounds = preservedBounds;
    currentId = state.id;
    if (state.readOnly) tools.deactivatePoints();
    else
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
          if (
            currentId !== state.id ||
            String(error) === "Image replaced or cancelled."
          )
            return;
          annotationError = `Annotations could not load: ${String(error)}`;
          $("status").textContent = annotationError;
          $("status").classList.add("error");
        },
      );
  }
  currentImage = state;
  // OSD has no public setter. This is its eviction target; visible tiles stay resident.
  (
    viewer.tileCache as OpenSeadragon.TileCache & { _maxCacheItemCount: number }
  )._maxCacheItemCount = imageTileCacheCount(state.channels?.length || 3);
  dimensionNavigation.setImage(
    state,
    sameDimensions || previousImage?.id === state.id,
  );
  $("points-button").setAttribute(
    "title",
    state.readOnly ? "Annotations unavailable while browsing" : "Multi-point",
  );
  $<HTMLButtonElement>("points-button").disabled = Boolean(state.readOnly);
  colorPanel.setChannels(
    (state.channels ?? []).map((channel, index) => ({
      id: String(index),
      ...channel,
    })),
    sameDimensions || previousImage?.id === state.id,
  );
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
  const errorStatus =
    saveError ||
    (state.phase === "error"
      ? state.error || "Could not load image"
      : annotationError || (state.phase !== "cancelled" ? tileError : null));
  const status =
    errorStatus ||
    (saving
      ? "Saving annotations…"
      : state.phase === "cancelled"
        ? "Cancelled"
        : saveNotice ||
          (state.phase === "loading"
            ? state.previewReady
              ? "Loading detail…"
              : state.width > 0
                ? "Building preview…"
                : "Reading image…"
            : state.readOnly
              ? "Browsing only · annotations unavailable"
              : ""));
  if ($("status").textContent !== status) $("status").textContent = status;
  $("status").title = errorStatus || saveNotice || state.detail || status;
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
    if (!state.channelMode && state.channels?.length) {
      histogramLoad = new AbortController();
      void loadChannelHistograms(
        `${tileUrl(state.id)}/preview.jpg`,
        state.channels.length,
        histogramLoad.signal,
      ).then(
        (bins) => {
          if (currentId === state.id) colorPanel.setHistograms(bins);
        },
        () => {},
      );
    }
    const viewportBounds = pendingViewportBounds;
    pendingViewportBounds = null;
    viewer.addTiledImage({
      tileSource: trackDownloads(
        colorRenderer!.wrapSource(
          preview(state, tileUrl),
          state,
          true,
          tileUrl(state.id),
        ),
      ),
      ...transform,
      width: 1,
      index: viewer.world.getItemCount(),
      opacity: sameDimensions ? 0 : 1,
      preload: true,
      success: (event) => {
        if (currentId !== state.id) {
          viewer.world.removeItem(
            (event as unknown as { item: OpenSeadragon.TiledImage }).item,
          );
          return;
        }
        const item = (event as unknown as { item: OpenSeadragon.TiledImage })
          .item;
        pendingPreview = item;
        if (!sameDimensions || viewer.world.getItemCount() === 1) {
          if (viewportBounds) viewer.viewport.fitBounds(viewportBounds, true);
          else cancelInitialFit = fitInitialImage(viewer);
        }
        const display = () => {
          // OSD can report fully loaded after its only tile failed.
          if (!item.getFullyLoaded() || !loadedImages.has(item)) return;
          item.removeHandler("fully-loaded-change", display);
          if (currentId !== state.id) {
            viewer.world.removeItem(item);
            return;
          }
          pendingPreview = null;
          // Keep the previous plane visible until all channels of this preview are ready.
          const previous = Array.from(
            { length: viewer.world.getItemCount() },
            (_, index) => viewer.world.getItemAt(index),
          );
          item.setOpacity(1);
          for (const old of previous)
            if (old !== item) viewer.world.removeItem(old);
          previewDisplayed = true;
          colorPanel.setEnabled(Boolean(state.channels?.length));
          document
            .querySelectorAll<HTMLButtonElement>("[data-image-control]")
            .forEach((button) => (button.disabled = false));
          updatePointTool();
          if (currentImage?.id === state.id && currentImage.tilesReady)
            addPyramid(currentImage);
        };
        item.addHandler("fully-loaded-change", display);
        display();
      },
      error: () => {
        if (currentId === state.id)
          showError("Could not display the image preview.");
      },
    });
  }
  if (state.tilesReady && previewDisplayed) addPyramid(state);
}

function addPyramid(state: ImageState) {
  if (pyramidAdded || state.width <= 0 || state.height <= 0) return;
  pyramidAdded = true;
  viewer.addTiledImage({
    tileSource: trackDownloads(
      colorRenderer!.wrapSource(
        pyramid(state, tileUrl),
        state,
        false,
        tileUrl(state.id),
      ),
    ),
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
    selectPlane: (id, position, z, time, hint, projection) =>
      invoke<ImageState>("select_plane", {
        id,
        position,
        z,
        time,
        navigationAxis: hint?.axis,
        navigationDirection: hint?.direction,
        projection,
      }),
    state: () => invoke<ImageState | null>("image_state"),
    cancel: () => invoke("cancel_load"),
    listen: native
      ? (changed) =>
          listen<ImageState>("image-state", (event) => changed(event.payload))
      : undefined,
  },
  showImage,
  showError,
);

let selectedPath: string | null = null;
let folderError: string | null = null;
const folder = new FolderNavigation(
  (path) => invoke<string[]>("folder_images", { path }),
  updateNavigation,
  (message) => {
    folderError = message;
    updateNavigation();
  },
);

function updateNavigation() {
  for (const [id, direction, label] of [
    ["previous-image-button", -1, "Previous image"],
    ["next-image-button", 1, "Next image"],
  ] as const) {
    const button = $<HTMLButtonElement>(id);
    button.disabled = !folder.adjacent(direction);
    button.title = folderError ? `${label}: ${folderError}` : label;
  }
}

async function navigateImage(direction: -1 | 1) {
  if (!native || saving || dialogOpen) return;
  if (dirty && !(await discardChanges())) return;
  const path = folder.adjacent(direction);
  if (path) await openPath(path, true);
}

async function openPath(path: string, checked = false) {
  if (!checked && !(await discardChanges())) return;
  selectedPath = path;
  folderError = null;
  folder.select(path);
  resetViewer();
  $("viewer").dataset.phase = "loading";
  $("filename").textContent = path.split(/[\\/]/).pop() || path;
  $("filename").title = $("filename").textContent || "";
  $("status").textContent = "Opening…";
  $("status").classList.remove("error");
  setLoading(true);
  await loader.open(path);
}

async function chooseImage() {
  if (!native || dialogOpen || !(await discardChanges())) return;
  dialogOpen = true;
  let path: string | string[] | null = null;
  try {
    path = await open({
      multiple: false,
      directory: false,
      filters: [
        {
          name: "Images",
          extensions: await invoke<string[]>("image_extensions"),
        },
      ],
    });
  } catch (error) {
    showError(String(error));
  } finally {
    dialogOpen = false;
  }
  if (typeof path === "string") await openPath(path, true);
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
$("previous-image-button").addEventListener(
  "click",
  () => void navigateImage(-1),
);
$("next-image-button").addEventListener("click", () => void navigateImage(1));
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
  if (dialogOpen || saving || $<HTMLDialogElement>("settings-dialog").open)
    return;
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "n") {
    event.preventDefault();
    if (native) void invoke("new_window").catch(showError);
  } else if (
    (event.ctrlKey || event.metaKey) &&
    event.key.toLowerCase() === "o"
  ) {
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
      if ($<HTMLDialogElement>("settings-dialog").open) {
        if ($<HTMLButtonElement>("settings-close").disabled) {
          event.preventDefault();
          return;
        }
        settings.close();
      }
      if (dirty || saving || dialogOpen) {
        event.preventDefault();
        if (await discardChanges()) {
          dirty = false;
          await windowHandle.close();
        }
      }
    })
    .catch(showError);
  for (const prefix of ["", "settings-"]) {
    $(`${prefix}minimize-button`).addEventListener(
      "click",
      () => void windowHandle.minimize().catch(showError),
    );
    $(`${prefix}maximize-button`).addEventListener(
      "click",
      () => void windowHandle.toggleMaximize().catch(showError),
    );
    $(
      prefix ? "settings-window-close-button" : "close-button",
    ).addEventListener(
      "click",
      () => void windowHandle.close().catch(showError),
    );
  }
  const syncMaximize = async () => {
    const maximized = await windowHandle.isMaximized();
    for (const id of ["maximize-button", "settings-maximize-button"]) {
      const button = $(id);
      button.innerHTML = icon(maximized ? "restore" : "maximize");
      button.title = maximized ? "Restore" : "Maximize";
      button.setAttribute("aria-label", button.title);
    }
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
  void invoke<string | null>("startup_image")
    .then((path) => (path ? openPath(path, true) : loader.restore()))
    .catch(showError);
} else {
  $<HTMLButtonElement>("open-button").disabled = true;
  $("open-button").title = "Open images in the desktop app";
  document
    .querySelectorAll<HTMLButtonElement>(".window-button")
    .forEach((button) => (button.disabled = true));
}

bindUpdates(
  prepareUpdate,
  () => {
    saving = false;
    dialogOpen = false;
    updatePointTool();
  },
  settings,
);

window.addEventListener("beforeunload", () => {
  loader.dispose();
  annotationViewer.dispose();
  colorRenderer?.dispose();
  viewer.destroy();
});
