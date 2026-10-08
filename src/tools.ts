import { icon } from "./icons";
import type { AnnotationSettings } from "./annotations";
import {
  defaultPointSettings,
  pointChoices,
  pointSymbol,
  type PointSettings,
} from "./point-settings";
import {
  adjustDisplay,
  displayValue,
  percentileRange,
  defaultDisplaySettings,
  type DisplaySettings,
} from "./display-settings";

const options = (values: string[], selected: string) =>
  values
    .map(
      (value) =>
        `<option${value === selected ? " selected" : ""}>${value}</option>`,
    )
    .join("");
const slider = (
  key: keyof DisplaySettings,
  label: string,
  min: number,
  max: number,
) => `
  <label class="adjustment" for="display-${key}">
    <span>${label}</span>
    <input id="display-${key}" data-display="${key}" type="range" min="${min}" max="${max}" value="${defaultDisplaySettings[key]}">
    <output id="value-${key}" for="display-${key}">${defaultDisplaySettings[key]}</output>
  </label>`;
const closeButton = `<button class="tool panel-close" type="button" aria-label="Close tool options" title="Close (Escape)">${icon("cancel")}</button>`;

export const toolPanels = `
  <aside id="tool-panels" aria-label="Tool options" hidden>
    <section id="brightness-panel" class="tool-panel" aria-labelledby="brightness-title" hidden>
      <header class="panel-heading"><h2 id="brightness-title">Display range</h2>${closeButton}</header>
      <div class="histogram-container">
      <svg class="range-graph" viewBox="0 0 255 64" preserveAspectRatio="none" role="img" aria-label="Approximate display histogram">
        <path class="graph-grid" d="M63.75 0v64M127.5 0v64M191.25 0v64M0 32h255"/>
        <title id="histogram-description">Open an image to show its approximate histogram</title>
        <path id="histogram-bars"/>
        <path id="range-line" d="M0 64L255 0"/>
        <line id="cutoff-minimum" class="histogram-cutoff" x1="0" x2="0" y1="0" y2="64"/>
        <line id="cutoff-maximum" class="histogram-cutoff" x1="255" x2="255" y1="0" y2="64"/>
      </svg>
      <button id="histogram-scale" class="text-button histogram-scale" type="button" aria-label="Logarithmic histogram scale" aria-pressed="false" title="Switch to logarithmic scale">Linear</button>
      </div>
      <div class="range-labels"><span id="range-minimum">0</span><span id="range-maximum">255</span></div>
      ${slider("minimum", "Minimum", 0, 254)}
      ${slider("maximum", "Maximum", 1, 255)}
      <footer class="panel-footer display-presets">${[0.1, 0.5, 1].map((tail) => `<button class="text-button percentile-button" data-percentile="${tail}" title="Set display range to the ${tail}–${100 - tail} percentiles" disabled>${tail}–${100 - tail}</button>`).join("")}<button id="reset-display" class="text-button" type="button">${icon("reset")}Reset</button></footer>
    </section>
    <section id="points-panel" class="tool-panel" aria-labelledby="points-title" hidden>
      <header class="panel-heading"><h2 id="points-title">Multi-point</h2>${closeButton}</header>
      <div class="point-options">
        ${(["type", "color", "size"] as const).map((key) => `<div class="point-option-row"><span id="point-${key}-label">${key[0].toUpperCase() + key.slice(1)}</span><div id="point-${key}" class="point-choices" role="group" aria-labelledby="point-${key}-label"></div></div>`).join("")}
      </div>
      <div class="point-flags">
        <label><input id="point-labels" type="checkbox" checked>Label points</label>
        <label><input id="point-all-slices" type="checkbox">Show on all slices</label>
      </div>
      <div class="point-counter">
        <label for="point-counter">Counter<select id="point-counter">${options(
          Array.from({ length: 10 }, (_, i) => String(i)),
          "0",
        )}</select></label>
        <span>Points <output id="point-count">0</output></span>
      </div>
    </section>
  </aside>`;

export function bindTools(
  onDisplayChange: (settings: DisplaySettings) => void,
  onPointsChange: (key?: keyof AnnotationSettings) => void = () => {},
) {
  const container = document.querySelector<HTMLElement>("#tool-panels")!;
  const names = ["brightness", "points"] as const;
  type PanelName = (typeof names)[number];
  const openPanels = new Set<PanelName>();
  const setOpen = (name: PanelName, open: boolean) => {
    if (open) openPanels.add(name);
    else openPanels.delete(name);
    container.hidden = openPanels.size === 0;
    const button = document.getElementById(`${name}-button`)!;
    button.setAttribute("aria-pressed", String(open));
    button.setAttribute("aria-expanded", String(open));
    document.getElementById(`${name}-panel`)!.hidden = !open;
    if (name === "points") onPointsChange();
  };
  const close = () => {
    // Escape closes the focused panel, otherwise the most recently opened one.
    const focused = names.find(
      (name) =>
        document
          .getElementById(`${name}-panel`)!
          .contains(document.activeElement) ||
        document.activeElement?.id === `${name}-button`,
    );
    const name =
      focused && openPanels.has(focused) ? focused : [...openPanels].at(-1);
    if (!name) return false;
    setOpen(name, false);
    document.getElementById(`${name}-button`)!.focus();
    return true;
  };
  for (const name of names) {
    document
      .getElementById(`${name}-button`)!
      .addEventListener("click", () => setOpen(name, !openPanels.has(name)));
    document
      .querySelector(`#${name}-panel .panel-close`)!
      .addEventListener("click", () => {
        setOpen(name, false);
        document.getElementById(`${name}-button`)!.focus();
      });
  }

  let display = { ...defaultDisplaySettings };
  let bins: number[] | null = null;
  let logarithmic = false;
  const refreshDisplay = () => {
    for (const key of Object.keys(display) as (keyof DisplaySettings)[]) {
      const input = document.getElementById(
        `display-${key}`,
      ) as HTMLInputElement;
      input.value = String(display[key]);
      input.style.setProperty(
        "--fill",
        `${((display[key] - Number(input.min)) / (Number(input.max) - Number(input.min))) * 100}%`,
      );
      document.getElementById(`value-${key}`)!.textContent = String(
        display[key],
      );
    }
    for (const key of ["minimum", "maximum"] as const) {
      const line = document.getElementById(`cutoff-${key}`)!;
      line.setAttribute("x1", String(display[key]));
      line.setAttribute("x2", String(display[key]));
    }
    document
      .getElementById("range-line")!
      .setAttribute(
        "d",
        Array.from(
          { length: 256 },
          (_, x) =>
            `${x === 0 ? "M" : "L"}${x} ${64 * (1 - displayValue(x, display))}`,
        ).join(""),
      );
    onDisplayChange(display);
  };
  container
    .querySelectorAll<HTMLInputElement>("[data-display]")
    .forEach((input) => {
      input.addEventListener("input", () => {
        display = adjustDisplay(
          display,
          input.dataset.display as keyof DisplaySettings,
          input.valueAsNumber,
        );
        refreshDisplay();
      });
    });
  document.getElementById("reset-display")!.addEventListener("click", () => {
    display = { ...defaultDisplaySettings };
    refreshDisplay();
  });

  container
    .querySelectorAll<HTMLButtonElement>("[data-percentile]")
    .forEach((button) => {
      button.addEventListener("click", () => {
        if (!bins) return;
        display = {
          ...defaultDisplaySettings,
          ...percentileRange(bins, Number(button.dataset.percentile)),
        };
        refreshDisplay();
      });
    });
  const setHistogram = (values: number[] | null) => {
    bins = values;
    const peak = values ? Math.max(...values) : 0;
    document
      .getElementById("histogram-bars")!
      .setAttribute(
        "d",
        values && peak > 0
          ? `M0 64${values.map((count, x) => `L${x} ${64 - 62 * (logarithmic ? Math.log1p(count) / Math.log1p(peak) : count / peak)}`).join("")}L255 64Z`
          : "",
      );
    container
      .querySelectorAll<HTMLButtonElement>("[data-percentile]")
      .forEach((button) => (button.disabled = !peak));
    document.getElementById("histogram-description")!.textContent = values
      ? `Approximate 8-bit display luminance histogram from a preview sample; ${logarithmic ? "logarithmic" : "linear"} counts`
      : "Histogram unavailable";
  };
  const scaleButton = document.getElementById("histogram-scale")!;
  scaleButton.addEventListener("click", () => {
    logarithmic = !logarithmic;
    scaleButton.textContent = logarithmic ? "Log" : "Linear";
    scaleButton.setAttribute("aria-pressed", String(logarithmic));
    scaleButton.title = `Switch to ${logarithmic ? "linear" : "logarithmic"} scale`;
    setHistogram(bins);
  });
  const setImageReady = (ready: boolean) => {
    container
      .querySelectorAll<HTMLInputElement | HTMLButtonElement>(
        "[data-display], #reset-display",
      )
      .forEach((control) => (control.disabled = !ready));
  };
  const resetImage = () => {
    display = { ...defaultDisplaySettings };
    setHistogram(null);
    setImageReady(false);
    document.getElementById("histogram-description")!.textContent =
      "Histogram appears when an image preview is available";
    refreshDisplay();
  };
  resetImage();

  let point = { ...defaultPointSettings };
  const keys = ["type", "color", "size"] as const;
  for (const key of keys) {
    const group = document.getElementById(`point-${key}`)!;
    for (const choice of pointChoices(key, point)) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "tool point-choice";
      button.dataset.value = choice.value;
      button.setAttribute("aria-label", choice.value);
      button.addEventListener("click", () => {
        point = { ...point, [key]: choice.value } as PointSettings;
        refreshPoints();
        onPointsChange(key);
      });
      group.append(button);
    }
  }
  function refreshPoints() {
    for (const key of keys) {
      const buttons = document.getElementById(`point-${key}`)!.children;
      pointChoices(key, point).forEach((choice, index) => {
        const button = buttons[index] as HTMLButtonElement;
        button.innerHTML = pointSymbol(choice.settings);
        button.setAttribute("aria-pressed", String(choice.selected));
        button.title = `${choice.value} · ${choice.settings.size} ${choice.settings.color.toLowerCase()} ${choice.settings.type.toLowerCase()}`;
      });
    }
  }
  refreshPoints();
  for (const [id, key] of [
    ["point-labels", "labels"],
    ["point-all-slices", "allSlices"],
    ["point-counter", "counter"],
  ] as const) {
    document
      .getElementById(id)!
      .addEventListener("change", () => onPointsChange(key));
  }
  return {
    close,
    resetImage,
    setImageReady,
    setHistogram,
    pointsActive: () => openPanels.has("points"),
    deactivatePoints: () => setOpen("points", false),
    setPointSettings: (settings: AnnotationSettings) => {
      point = {
        type: settings.type,
        color: settings.color,
        size: settings.size,
      };
      (document.getElementById("point-labels") as HTMLInputElement).checked =
        settings.labels;
      (
        document.getElementById("point-all-slices") as HTMLInputElement
      ).checked = settings.allSlices;
      (
        document.querySelector("select#point-counter") as HTMLSelectElement
      ).value = String(settings.counter);
      refreshPoints();
    },
    pointSettings: () => ({
      ...point,
      labels: (document.getElementById("point-labels") as HTMLInputElement)
        .checked,
      allSlices: (
        document.getElementById("point-all-slices") as HTMLInputElement
      ).checked,
      counter: Number(
        (document.querySelector("select#point-counter") as HTMLSelectElement)
          .value,
      ),
    }),
  };
}
