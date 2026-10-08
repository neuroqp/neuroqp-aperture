import { icon } from "./icons";
import {
  defaultPointSettings,
  pointChoices,
  pointSymbol,
  type PointSettings,
} from "./point-settings";
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
    <span>${label}</span>
    <input id="display-${key}" data-display="${key}" type="range" min="${min}" max="${max}" value="${defaultDisplaySettings[key]}">
    <output id="value-${key}" for="display-${key}">${defaultDisplaySettings[key]}</output>
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

export function bindTools() {
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

  refreshDisplay();

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
  return { close };
}
