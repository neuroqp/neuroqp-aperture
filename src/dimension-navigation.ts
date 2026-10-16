import { icon } from "./icons";
import {
  PlaneNavigation,
  dimensionAxis,
  projectsAxis,
  type DimensionAxis,
  type ImageState,
  type NavigationHint,
  type ProjectionMode,
} from "./image";

export const dimensionPanelMarkup = `
  <section id="navigator-panel" class="tool-panel" aria-labelledby="navigator-title" hidden>
    <header class="panel-heading"><h2 id="navigator-title">Dimensions</h2><button class="tool panel-close" type="button" aria-label="Close dimensions" title="Close (Escape)">${icon("cancel")}</button></header>
    <label class="dimension-axis">Scroll <select id="dimension-axis" aria-label="Dimension"><option value="position">Position</option><option value="z">Z</option><option value="time">Time</option><option value="channel">Channel</option></select><output id="dimension-count" aria-live="polite"></output></label>
    <div id="dimension-scroll" class="dimension-scroll"><button id="dimension-previous" class="tool" type="button" aria-label="Previous dimension value">${icon("previous")}</button><input id="dimension-range" type="range" min="1" max="1" value="1" aria-label="Position"><button id="dimension-next" class="tool" type="button" aria-label="Next dimension value">${icon("next")}</button></div>
    <div id="dimension-name" class="dimension-name"></div>
    <div id="dimension-projections" class="dimension-projections" role="group" aria-label="Maximum intensity projection" hidden>
      <label title="Maximum intensity across all Z planes"><input id="dimension-max-z" type="checkbox">Z max</label>
      <label title="Maximum intensity across all time points"><input id="dimension-max-time" type="checkbox">Time max</label>
    </div>
    <p id="dimension-projection-status" class="dimension-scope" role="status" hidden></p>
    <p id="dimension-calibration" class="dimension-scope" hidden></p>
    <p id="dimension-display" class="dimension-scope" hidden>16-bit display ranges adapt to each frame and channel. Brightness across frames is not quantitative.</p>
    <p id="dimension-scope" class="dimension-scope" hidden>ND2 browsing only. Annotation editing and saving are unavailable.</p>
  </section>`;

export function bindDimensionNavigation(
  selectPlane: (
    position: number,
    z: number,
    time: number,
    hint?: NavigationHint,
    projection?: ProjectionMode,
  ) => Promise<boolean>,
  channelChanged: () => void,
) {
  const get = <T extends HTMLElement = HTMLElement>(id: string) =>
    document.getElementById(id)! as T;
  const axis = get<HTMLSelectElement>("dimension-axis");
  const range = get<HTMLInputElement>("dimension-range");
  const previous = get<HTMLButtonElement>("dimension-previous");
  const next = get<HTMLButtonElement>("dimension-next");
  const maxZ = get<HTMLInputElement>("dimension-max-z");
  const maxTime = get<HTMLInputElement>("dimension-max-time");
  let image: ImageState | null = null;
  let channel = -1;
  const selectedAxis = () => {
    const plane = planeNavigation.selection;
    const displayed =
      image?.dimensions && plane
        ? {
            ...image,
            projection: planeNavigation.projection,
            dimensions: {
              ...image.dimensions,
              position: plane.position,
              zIndex: plane.z,
              timeIndex: plane.time,
            },
          }
        : image;
    return dimensionAxis(displayed, axis.value as DimensionAxis, channel);
  };
  const render = () => {
    const selected = selectedAxis();
    const spatial = Boolean(image?.dimensions);
    for (const option of axis.options)
      option.disabled = option.value !== "channel" && !spatial;
    const enabled =
      axis.value === "channel" ? Boolean(image?.previewReady) : spatial;
    axis.disabled = !image;
    const projected = Boolean("projected" in selected && selected.projected);
    range.disabled = !enabled || selected.count <= 1 || projected;
    previous.disabled = range.disabled || selected.index <= 0;
    next.disabled = range.disabled || selected.index >= selected.count - 1;
    range.max = String(selected.count);
    range.value = String(selected.index + 1);
    range.setAttribute(
      "aria-label",
      axis.selectedOptions[0].textContent || "Dimension",
    );
    range.setAttribute(
      "aria-valuetext",
      projected
        ? `Maximum of ${selected.count}, ${selected.name}`
        : `${selected.index + 1} of ${selected.count}, ${selected.name}`,
    );
    get("dimension-count").textContent = projected
      ? `Max of ${selected.count}`
      : `${selected.index + 1} / ${selected.count}`;
    get("dimension-name").textContent = selected.name;
    get("dimension-projections").hidden = !spatial;
    maxZ.disabled = !spatial || (image?.dimensions?.z ?? 1) <= 1;
    maxTime.disabled = !spatial || (image?.dimensions?.time ?? 1) <= 1;
    maxZ.checked = projectsAxis(planeNavigation.projection, "z");
    maxTime.checked = projectsAxis(planeNavigation.projection, "time");
    const projectionStatus = get("dimension-projection-status");
    projectionStatus.hidden = planeNavigation.projection === "none";
    projectionStatus.textContent =
      maxZ.checked && maxTime.checked
        ? "Maximum intensity across Z and Time"
        : maxZ.checked
          ? "Maximum intensity across Z"
          : "Maximum intensity across Time";
    get("dimension-scope").hidden = !image?.readOnly;
    get("dimension-display").hidden = !image?.displayRanges?.length;
    const calibration = image?.calibration;
    const calibrationText = [
      calibration?.pixelWidthUm
        ? `Pixel ${Number(calibration.pixelWidthUm.toPrecision(4))}${calibration.pixelHeightUm ? ` × ${Number(calibration.pixelHeightUm.toPrecision(4))}` : " X"} µm`
        : "",
      calibration?.zStepUm
        ? `Z step ${Number(calibration.zStepUm.toPrecision(4))} µm`
        : "",
      calibration?.timeIncrementSeconds
        ? `Nominal interval ${Number(calibration.timeIncrementSeconds.toPrecision(4))} s`
        : "",
    ]
      .filter(Boolean)
      .join(" · ");
    get("dimension-calibration").textContent = calibrationText;
    get("dimension-calibration").hidden = !calibrationText;
    get<HTMLButtonElement>("navigator-button").disabled =
      !spatial && !image?.channels?.length;
  };
  const planeNavigation = new PlaneNavigation(selectPlane, render);
  const choose = (value: number) => {
    if (range.disabled) return;
    const selected = selectedAxis();
    const index = Math.max(0, Math.min(selected.count - 1, value));
    if (selected.index === index) return;
    if (axis.value === "channel") {
      channel = index - 1;
      render();
      channelChanged();
    } else {
      planeNavigation.select(axis.value as "position" | "z" | "time", index);
    }
  };
  const project = () => {
    const mode = maxZ.checked
      ? maxTime.checked
        ? "zTime"
        : "z"
      : maxTime.checked
        ? "time"
        : "none";
    planeNavigation.setProjection(mode);
  };
  maxZ.addEventListener("change", project);
  maxTime.addEventListener("change", project);
  axis.addEventListener("change", render);
  range.addEventListener("input", () => void choose(Number(range.value) - 1));
  previous.addEventListener(
    "click",
    () => void choose(Number(range.value) - 2),
  );
  next.addEventListener("click", () => void choose(Number(range.value)));
  get("dimension-scroll").addEventListener(
    "wheel",
    (event) => {
      if (range.disabled || event.ctrlKey || event.metaKey || !event.deltaY)
        return;
      event.preventDefault();
      event.stopPropagation();
      void choose(Number(range.value) - 1 + Math.sign(event.deltaY));
    },
    { passive: false },
  );
  render();
  return {
    channel: () => channel,
    setImage(next: ImageState | null, preserve = false) {
      const dimensionsAppeared =
        !image?.dimensions && Boolean(next?.dimensions);
      image = next;
      planeNavigation.setPlane(
        next?.dimensions
          ? {
              position: next.dimensions.position,
              z: next.dimensions.zIndex,
              time: next.dimensions.timeIndex,
            }
          : null,
        preserve,
        next?.projection ?? "none",
      );
      if (!preserve) channel = -1;
      if (!preserve || dimensionsAppeared)
        axis.value = next?.dimensions ? "position" : "channel";
      if (channel >= (next?.channels?.length ?? 0)) channel = -1;
      render();
    },
  };
}
