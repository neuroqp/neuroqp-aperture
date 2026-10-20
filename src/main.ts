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
import { bindWindowControls } from "./window-controls";
import { channelHistograms, loadChannelHistograms } from "./display";
import { bindColorPanel } from "./color-panel";
import { bindDimensionNavigation } from "./dimension-navigation";
import { visibleChannelSettings } from "./color-settings";
import { bindColorRenderer } from "./color-renderer";
import { createGpuColorDrawer } from "./gpu-color-drawer";
import { fitInitialImage } from "./initial-fit";
import { statusFor } from "./status";
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

// Everything tied to one native image id; a new id starts from a fresh value.
const blankImage = () => ({
  id: null as string | null,
  image: null as ImageState | null,
  rois: [] as PointRoi[],
  annotationsReady: false,
  dirty: false,
  savedPath: null as string | null,
  annotationError: null as string | null,
  tileError: null as string | null,
  saveError: null as string | null,
  saveNotice: null as string | null,
  previewAdded: false,
  previewDisplayed: false,
  pendingPreview: null as OpenSeadragon.TiledImage | null,
  pyramidAdded: false,
  histogramLoad: null as AbortController | null,
  cancelInitialFit: undefined as (() => void) | undefined,
});
let current = blankImage();
// Shown until the next image state arrives.
let statusMessage: { text: string; error: boolean } | null = null;
let saving = false;
// Freeze edits while an update stops image work and installs.
let updating = false;
let dialogOpen = false;
let loading = false;
let selectedPath: string | null = null;
let folderError: string | null = null;
let pendingViewportBounds: OpenSeadragon.Rect | null = null;
const downloads = new Set<OpenSeadragon.ImageJob>();
const loadedImages = new WeakSet<OpenSeadragon.TiledImage>();
const saveDialog = $<HTMLDialogElement>("save-dialog");
const settingsDialog = $<HTMLDialogElement>("settings-dialog");
const frozen = () => saving || updating;
const blocked = () => frozen() || dialogOpen || settingsDialog.open;
const tileUrl = (id: string) => convertFileSrc(id, "aperture");

let colorRenderer: ReturnType<typeof bindColorRenderer> | undefined;
const colorPanel = bindColorPanel(() => colorRenderer?.refresh());
const tools = bindTools(updatePointTool, colorPanel);
const settings = bindSettings();
bindTheme();
const native = isTauri();
const windowHandle = native ? getCurrentWindow() : null;
const dimensionNavigation = bindDimensionNavigation(
  async (position, z, time, hint, projection) => {
    if (!native || blocked()) return false;
    setLoading(true);
    return loader.selectPlane(position, z, time, hint, projection);
  },
  () => colorRenderer?.refresh(),
);
const displayColors = () =>
  visibleChannelSettings(colorPanel.settings(), dimensionNavigation.channel());
// Keep display failures across later status updates for the same image.
const displayError = (message: string) => {
  current.tileError = message;
  renderStatus();
};
const GpuColorDrawer = createGpuColorDrawer(
  OpenSeadragon,
  displayColors,
  (message) => queueMicrotask(() => displayError(message)),
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
viewer.addHandler("tile-loaded", (event) => {
  loadedImages.add(event.tiledImage);
});

colorRenderer = bindColorRenderer(
  viewer,
  displayColors,
  displayError,
  (imageId, data) => {
    if (current.id === imageId)
      colorPanel.setHistograms(
        channelHistograms(data.planes, data.width, data.height),
      );
  },
);

const annotationViewer = new AnnotationViewer(viewer, {
  add: (point) => {
    if (current.image)
      editAnnotations(
        addPoint(current.rois, point, tools.pointSettings(), current.image),
      );
  },
  move: (reference, point) => {
    if (current.image)
      editAnnotations(movePoint(current.rois, reference, point, current.image));
  },
  delete: (reference) => editAnnotations(deletePoint(current.rois, reference)),
  active: () => tools.pointsActive(),
  editable: () => current.annotationsReady && !frozen(),
});

async function withDialog<T>(show: () => Promise<T>) {
  dialogOpen = true;
  try {
    return await show();
  } finally {
    dialogOpen = false;
  }
}

function showModal(dialog: HTMLDialogElement) {
  dialog.returnValue = "cancel";
  return new Promise<string>((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue), {
      once: true,
    });
    dialog.showModal();
  });
}

function setRois(rois: PointRoi[], edited: boolean) {
  current.rois = rois;
  if (edited) current.dirty = true;
  annotationViewer.setRois(rois);
}

function editAnnotations(next: PointRoi[] | null) {
  if (!next || next === current.rois || !current.annotationsReady || frozen())
    return;
  setRois(next, true);
  updatePointTool();
}

function updatePointTool(key?: keyof AnnotationSettings) {
  const editable = current.annotationsReady && !frozen();
  if (key === "counter" && editable) {
    setRois(selectCounter(current.rois, tools.pointSettings().counter), false);
  } else if (key && editable && key !== "allSlices" && key !== "counter") {
    const next = updatePointRois(current.rois, key, tools.pointSettings()[key]);
    if (next !== current.rois) setRois(next, true);
  }
  document
    .querySelectorAll<HTMLButtonElement | HTMLInputElement | HTMLSelectElement>(
      ".point-choice, #point-labels, #point-all-slices, #point-counter, #point-counter-options button[data-counter]",
    )
    .forEach((control) => {
      control.disabled = !editable;
    });
  annotationViewer.updateInteraction();
  $("pan-button").classList.toggle("selected", !tools.pointsActive());
  const { rois, dirty } = current;
  $("point-count").textContent = String(
    counterCount(rois, tools.pointSettings().counter),
  );
  $("point-count").title =
    `${rois.reduce((sum, roi) => sum + roi.points.length, 0)} points total`;
  $("filename").classList.toggle("unsaved", dirty);
  $<HTMLButtonElement>("save-button").disabled =
    !editable || (!dirty && !rois.some((roi) => roi.points.length > 0));
  renderStatus();
}

function renderStatus() {
  const { text, title, error } = statusFor({
    ...current,
    message: statusMessage,
    saving,
  });
  const status = $("status");
  if (status.textContent !== text) status.textContent = text;
  status.title = title;
  status.classList.toggle("error", error);
}

async function discardChanges() {
  if (blocked()) return false;
  return (
    !current.dirty ||
    withDialog(() =>
      confirm("Discard unsaved point annotations? Save first to keep them.", {
        title: "Unsaved annotations",
        kind: "warning",
        okLabel: "Discard",
        cancelLabel: "Cancel",
      }),
    )
  );
}

async function prepareUpdate() {
  if (blocked()) return false;
  if (current.dirty) {
    const choice = await withDialog(() =>
      showModal($<HTMLDialogElement>("update-unsaved-dialog")),
    );
    if (choice === "save") {
      await saveAnnotations();
      if (current.dirty) return false;
    } else if (choice !== "discard") return false;
    // Keep discarded edits marked unsaved until exit; installation can still fail.
  }
  // Freeze edits before stopping image requests and joining native workers.
  updating = true;
  current.histogramLoad?.abort();
  abortDownloads();
  updatePointTool();
  return true;
}

async function saveAnnotations() {
  const session = current;
  const id = session.id;
  if (
    !native ||
    blocked() ||
    !id ||
    $<HTMLButtonElement>("save-button").disabled
  )
    return;
  saving = true;
  statusMessage = null;
  session.saveError = null;
  session.saveNotice = null;
  updatePointTool();
  try {
    const target = await invoke<AnnotationTarget>("annotation_target", {
      id,
      path: session.savedPath,
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
        return showModal(saveDialog);
      },
      pick: (defaultPath) =>
        save({
          title: "Save annotated TIFF as",
          defaultPath,
          filters: [{ name: "TIFF image", extensions: ["tif", "tiff"] }],
        }),
      inspect: (path) =>
        invoke<AnnotationTarget>("annotation_target", { id, path }),
      write: async (path, overwrite, acknowledgment) => {
        const result = await invoke<SaveResult>("save_annotations", {
          id,
          path,
          rois: session.rois,
          overwrite,
          acknowledgment,
        });
        if (result.saved) session.saveNotice = result.warning;
        return result;
      },
    });
    if (!path) return;
    session.savedPath = path;
    session.dirty = false;
    // Confirm the save now, even while detail is still loading.
    if (current === session)
      statusMessage = { text: session.saveNotice || "Saved", error: false };
  } catch (error) {
    session.saveError = String(error);
  } finally {
    saving = false;
    updatePointTool();
  }
}

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

function abortDownloads() {
  for (const job of downloads) job.abort();
  downloads.clear();
}

const tileSource = (state: ImageState, isPreview: boolean) =>
  trackDownloads(
    colorRenderer!.wrapSource(
      (isPreview ? preview : pyramid)(state, tileUrl),
      state,
      isPreview,
      tileUrl(state.id),
    ),
  );

// OSD passes the added item to addTiledImage callbacks without typing it.
const addedItem = (event: unknown) =>
  (event as { item: OpenSeadragon.TiledImage }).item;

const setImageControls = (enabled: boolean) =>
  document
    .querySelectorAll<HTMLButtonElement>("[data-image-control]")
    .forEach((button) => (button.disabled = !enabled));

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
  current.cancelInitialFit?.();
  current.histogramLoad?.abort();
  if (current.pendingPreview) viewer.world.removeItem(current.pendingPreview);
  current = blankImage();
  tools.setPointSettings(settingsForRois([], 0));
  annotationViewer.setImage(null);
  annotationViewer.setRois([]);
  updatePointTool();
  if (!preserveDisplay) viewer.close();
  abortDownloads();
  if (!preserveDisplay) {
    pendingViewportBounds = null;
    colorPanel.setChannels([]);
    dimensionNavigation.setImage(null);
  }
  colorPanel.setEnabled(false);
  setImageControls(false);
  $("dimensions").textContent = "";
}

function showError(message: string) {
  statusMessage = { text: message, error: true };
  setLoading(false);
  $("viewer").dataset.phase = "error";
  renderStatus();
}

function loadAnnotations(id: string) {
  const session = current;
  void invoke<PointRoi[]>("load_annotations", { id }).then(
    (loaded) => {
      if (current !== session) return;
      tools.setPointSettings(
        settingsForRois(loaded, tools.pointSettings().counter),
      );
      session.annotationsReady = true;
      setRois(loaded, false);
      updatePointTool();
    },
    (error) => {
      if (
        current !== session ||
        String(error) === "Image replaced or cancelled."
      )
        return;
      session.annotationError = `Annotations could not load: ${String(error)}`;
      renderStatus();
    },
  );
}

function showImage(state: ImageState) {
  statusMessage = null;
  if (!selectedPath && state.path) {
    selectedPath = state.path;
    folder.select(state.path);
  }
  const previous = current.image;
  const sameDimensions = Boolean(
    state.dimensions &&
    previous?.dimensions &&
    state.path &&
    state.path === previous.path,
  );
  // Plane changes within one file keep the viewport, channels and dimensions.
  const preserve = sameDimensions || previous?.id === state.id;
  if (current.id !== state.id) {
    const bounds = sameDimensions
      ? viewer.world.getItemCount() > 0
        ? viewer.viewport.getBounds()
        : pendingViewportBounds
      : null;
    resetViewer(sameDimensions);
    pendingViewportBounds = bounds;
    current.id = state.id;
    if (state.readOnly) tools.deactivatePoints();
    else loadAnnotations(state.id);
  }
  current.image = state;
  // OSD has no public setter. This is its eviction target; visible tiles stay resident.
  (
    viewer.tileCache as OpenSeadragon.TileCache & { _maxCacheItemCount: number }
  )._maxCacheItemCount = imageTileCacheCount(state.channels?.length || 3);
  dimensionNavigation.setImage(state, preserve);
  $("points-button").setAttribute(
    "title",
    state.readOnly ? "Browsing only · annotations unavailable" : "Multi-point",
  );
  $<HTMLButtonElement>("points-button").disabled = Boolean(state.readOnly);
  colorPanel.setChannels(
    (state.channels ?? []).map((channel, index) => ({
      id: String(index),
      ...channel,
    })),
    preserve,
  );
  if (state.width > 0) annotationViewer.setImage(state);
  $("filename").textContent = state.name;
  $("filename").classList.toggle("unsaved", current.dirty);
  $("viewer").dataset.phase = state.phase;
  $("filename").title = state.name;
  setLoading(state.phase === "loading", state.progress);
  renderStatus();
  const { degrees } = orientation(state.orientation);
  const [displayWidth, displayHeight] =
    degrees % 180 === 0
      ? [state.width, state.height]
      : [state.height, state.width];
  if (state.width > 0 && state.height > 0)
    $("dimensions").textContent =
      `${displayWidth.toLocaleString()} × ${displayHeight.toLocaleString()}`;

  if (state.previewReady && !current.previewAdded)
    addPreview(state, sameDimensions);
  if (state.tilesReady && current.previewDisplayed) addPyramid(state);
}

function addPreview(state: ImageState, sameDimensions: boolean) {
  const session = current;
  session.previewAdded = true;
  if (!state.channelMode && state.channels?.length) {
    session.histogramLoad = new AbortController();
    void loadChannelHistograms(
      `${tileUrl(state.id)}/preview.jpg`,
      state.channels.length,
      session.histogramLoad.signal,
    ).then(
      (bins) => {
        if (current === session) colorPanel.setHistograms(bins);
      },
      () => {},
    );
  }
  const viewportBounds = pendingViewportBounds;
  pendingViewportBounds = null;
  viewer.addTiledImage({
    tileSource: tileSource(state, true),
    ...orientation(state.orientation),
    width: 1,
    index: viewer.world.getItemCount(),
    opacity: sameDimensions ? 0 : 1,
    preload: true,
    success: (event) => {
      const item = addedItem(event);
      if (current !== session) {
        viewer.world.removeItem(item);
        return;
      }
      session.pendingPreview = item;
      if (!sameDimensions || viewer.world.getItemCount() === 1) {
        if (viewportBounds) viewer.viewport.fitBounds(viewportBounds, true);
        else session.cancelInitialFit = fitInitialImage(viewer);
      }
      const display = () => {
        // OSD can report fully loaded after its only tile failed.
        if (!item.getFullyLoaded() || !loadedImages.has(item)) return;
        item.removeHandler("fully-loaded-change", display);
        if (current !== session) {
          viewer.world.removeItem(item);
          return;
        }
        session.pendingPreview = null;
        // Keep the previous plane visible until all channels of this preview are ready.
        const previous = Array.from(
          { length: viewer.world.getItemCount() },
          (_, index) => viewer.world.getItemAt(index),
        );
        item.setOpacity(1);
        for (const old of previous)
          if (old !== item) viewer.world.removeItem(old);
        session.previewDisplayed = true;
        colorPanel.setEnabled(Boolean(state.channels?.length));
        setImageControls(true);
        updatePointTool();
        if (session.image?.tilesReady) addPyramid(session.image);
      };
      item.addHandler("fully-loaded-change", display);
      display();
    },
    error: () => {
      if (current === session)
        displayError("Could not display the image preview.");
    },
  });
}

function addPyramid(state: ImageState) {
  const session = current;
  if (session.pyramidAdded || state.width <= 0 || state.height <= 0) return;
  session.pyramidAdded = true;
  viewer.addTiledImage({
    tileSource: tileSource(state, false),
    ...orientation(state.orientation),
    width: 1,
    index: 1,
    success: (event) => {
      if (current !== session) viewer.world.removeItem(addedItem(event));
    },
    error: () => {
      if (current === session)
        displayError("Detail could not load. Preview remains available.");
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
  if (!native || !(await discardChanges())) return;
  const path = folder.adjacent(direction);
  if (path) await openPath(path, true);
}

async function openPath(path: string, checked = false) {
  if (!checked && !(await discardChanges())) return;
  selectedPath = path;
  folderError = null;
  folder.select(path);
  resetViewer();
  statusMessage = { text: "Opening…", error: false };
  renderStatus();
  $("viewer").dataset.phase = "loading";
  $("filename").textContent = path.split(/[\\/]/).pop() || path;
  $("filename").title = $("filename").textContent || "";
  setLoading(true);
  await loader.open(path);
}

async function chooseImage() {
  if (!native || !(await discardChanges())) return;
  let path: string | string[] | null = null;
  try {
    path = await withDialog(async () =>
      open({
        multiple: false,
        directory: false,
        filters: [
          {
            name: "Images",
            extensions: await invoke<string[]>("image_extensions"),
          },
        ],
      }),
    );
  } catch (error) {
    showError(String(error));
  }
  if (typeof path === "string") await openPath(path, true);
}

async function cancelLoad() {
  if (!loading) return;
  statusMessage = { text: "Cancelling…", error: false };
  renderStatus();
  $<HTMLButtonElement>("cancel-button").disabled = true;
  if (await loader.cancel()) {
    setLoading(false);
    // Without an image state, nothing else reports the cancellation.
    if (!current.image) statusMessage = { text: "Cancelled", error: false };
    renderStatus();
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
    current.id ? tileUrl(current.id) : null,
    $("viewer").dataset.phase,
    event.tile.url,
  );
  if (
    !message ||
    current.tileError === message ||
    current.tileError === "Could not display the image preview."
  )
    return;
  current.tileError = message;
  renderStatus();
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
  if (blocked()) return;
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
      if (settingsDialog.open) {
        if (settings.busy()) {
          event.preventDefault();
          return;
        }
        settings.close();
      }
      if (current.dirty || blocked()) {
        event.preventDefault();
        if (await discardChanges()) {
          current.dirty = false;
          await windowHandle.close();
        }
      }
    })
    .catch(showError);
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
}
bindWindowControls(windowHandle, showError);

bindUpdates(
  prepareUpdate,
  () => {
    updating = false;
    updatePointTool();
  },
  settings,
);

// Stop the native image-state listener; page listeners end with the document.
window.addEventListener("beforeunload", () => loader.dispose());
