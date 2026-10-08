import { icon } from "./icons";
import {
  adjustDisplay,
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
    <span>${label}</span><output id="value-${key}" for="display-${key}">${defaultDisplaySettings[key]}</output>
    <input id="display-${key}" data-display="${key}" type="range" min="${min}" max="${max}" value="${defaultDisplaySettings[key]}">
  </label>`;
const closeButton = `<button class="tool panel-close" type="button" aria-label="Close tool options" title="Close (Escape)">${icon("cancel")}</button>`;

export const toolPanels = `
  <aside id="tool-panels" aria-label="Tool options" hidden>
    <section id="brightness-panel" class="tool-panel" aria-labelledby="brightness-title" hidden>
      <header class="panel-heading"><h2 id="brightness-title">Brightness &amp; contrast</h2>${closeButton}</header>
      <svg class="range-graph" viewBox="0 0 240 64" preserveAspectRatio="none" role="img" aria-label="Display range preview">
        <path class="graph-grid" d="M60 0v64M120 0v64M180 0v64M0 32h240"/>
        <path id="range-line" d="M0 64L240 0"/>
      </svg>
      <div class="range-labels"><span id="range-minimum">0</span><span id="range-maximum">255</span></div>
      ${slider("minimum", "Minimum", 0, 254)}
      ${slider("maximum", "Maximum", 1, 255)}
      ${slider("brightness", "Brightness", -100, 100)}
      ${slider("contrast", "Contrast", 0, 200)}
      <footer class="panel-footer"><button id="reset-display" class="text-button" type="button">${icon("reset")}Reset</button></footer>
    </section>
    <section id="points-panel" class="tool-panel" aria-labelledby="points-title" hidden>
      <header class="panel-heading"><h2 id="points-title">Multi-point</h2>${closeButton}</header>
      <div class="point-appearance">
        <div class="point-fields">
          <label for="point-type">Type<select id="point-type">${options(["Cross", "Dot", "Hybrid", "Circle"], "Cross")}</select></label>
          <label for="point-color">Color<select id="point-color">${options(["Magenta", "Red", "Yellow", "Green", "Cyan", "Blue", "White", "Black"], "Magenta")}</select></label>
          <label for="point-size">Size<select id="point-size">${options(["Tiny", "Small", "Medium", "Large", "Extra large"], "Extra large")}</select></label>
        </div>
        <div class="point-preview" role="img" aria-label="Point appearance preview"><svg viewBox="0 0 48 48"><g id="point-marker"></g><text id="point-label" x="35" y="13">1</text></svg></div>
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

export function bindTools() {
  const container = document.querySelector<HTMLElement>("#tool-panels")!;
  const names = ["brightness", "points"] as const;
  let active: (typeof names)[number] | null = null;
  const setActive = (name: typeof active) => {
    active = name;
    container.hidden = name === null;
    for (const item of names) {
      const selected = item === name;
      const button = document.getElementById(`${item}-button`)!;
      button.setAttribute("aria-pressed", String(selected));
      button.setAttribute("aria-expanded", String(selected));
      document.getElementById(`${item}-panel`)!.hidden = !selected;
    }
  };
  const close = () => {
    if (!active) return false;
    const button = document.getElementById(`${active}-button`)!;
    setActive(null);
    button.focus();
    return true;
  };
  for (const name of names) {
    document
      .getElementById(`${name}-button`)!
      .addEventListener("click", () =>
        setActive(active === name ? null : name),
      );
  }
  container
    .querySelectorAll(".panel-close")
    .forEach((button) => button.addEventListener("click", close));

  let display = { ...defaultDisplaySettings };
  const refreshDisplay = () => {
    for (const key of Object.keys(display) as (keyof DisplaySettings)[]) {
      (document.getElementById(`display-${key}`) as HTMLInputElement).value =
        String(display[key]);
      document.getElementById(`value-${key}`)!.textContent = String(
        display[key],
      );
    }
    document.getElementById("range-minimum")!.textContent = String(
      display.minimum,
    );
    document.getElementById("range-maximum")!.textContent = String(
      display.maximum,
    );
    const low = (display.minimum / 255) * 240;
    const high = (display.maximum / 255) * 240;
    document
      .getElementById("range-line")!
      .setAttribute("d", `M0 64H${low}L${high} 0H240`);
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

  const updatePointPreview = () => {
    const type = (document.getElementById("point-type") as HTMLSelectElement)
      .value;
    const color = (
      document.getElementById("point-color") as HTMLSelectElement
    ).value.toLowerCase();
    const size = (document.getElementById("point-size") as HTMLSelectElement)
      .selectedIndex;
    const radius = [2, 3, 5, 7, 9][size];
    const cross = `<path d="M${24 - radius} 24h${radius * 2}M24 ${24 - radius}v${radius * 2}"/>`;
    const circle = `<circle cx="24" cy="24" r="${type === "Hybrid" ? radius / 2 : radius}"${type === "Dot" ? ' fill="currentColor" stroke="none"' : ""}/>`;
    document.getElementById("point-marker")!.innerHTML =
      type === "Cross" ? cross : type === "Hybrid" ? circle + cross : circle;
    container.querySelector<HTMLElement>(".point-preview")!.style.color = color;
    document.getElementById("point-label")!.style.display = (
      document.getElementById("point-labels") as HTMLInputElement
    ).checked
      ? ""
      : "none";
  };
  for (const id of [
    "point-type",
    "point-color",
    "point-size",
    "point-labels",
  ]) {
    document.getElementById(id)!.addEventListener("change", updatePointPreview);
  }
  updatePointPreview();
  return { close };
}
