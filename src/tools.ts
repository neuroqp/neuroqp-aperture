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
        <div class="counter-controls">
          <span id="point-counter-label">Counter</span>
          <div class="counter-picker">
            <button id="point-counter" class="counter-trigger" type="button" value="0" aria-labelledby="point-counter-label point-counter-value" aria-haspopup="listbox" aria-controls="point-counter-options" aria-expanded="false"><span id="point-counter-value">0</span>${icon("dropdown")}</button>
            <div id="point-counter-options" class="counter-options" role="listbox" aria-labelledby="point-counter-label" popover="manual" hidden>
              ${Array.from({ length: 100 }, (_, counter) => `<button id="counter-option-${counter}" type="button" role="option" data-counter="${counter}" aria-selected="${counter === 0}" tabindex="-1">${counter}</button>`).join("")}
            </div>
          </div>
          <div class="counter-help">
            <button id="counter-help-button" class="counter-help-button" type="button" aria-label="About counters" aria-describedby="counter-help-text" aria-controls="counter-help-text" aria-expanded="false">${icon("help")}</button>
            <div id="counter-help-text" class="counter-tooltip" role="tooltip" popover="manual" hidden>
              <strong>Counters group points into categories</strong>
              <p>Choose a counter from 0 to 99 to assign new points to that category. Points shows the selected category's total in this selection.</p>
              <p>A fresh selection using counter 0 labels points in order: 1, 2, 3. Selecting a higher counter switches to category labels, so points in counter 1 all read 1. Returning to 0 keeps category labels until you start a fresh selection.</p>
              <p>Label points only changes label visibility. Type and Size apply to the whole selection. Color applies in the initial counter 0 mode; afterward, colors are assigned by category.</p>
              <p>Compatible with ImageJ’s multi-point tool.</p>
            </div>
          </div>
        </div>
        <span>Points <output id="point-count">0</output></span>
      </div>
    </section>
  </aside>`;

export function bindTools(
  onDisplayChange: (settings: DisplaySettings) => void,
  onPointsChange: (key?: keyof AnnotationSettings) => void = () => {},
) {
  const container = document.querySelector<HTMLElement>("#tool-panels")!;
  const counter = document.getElementById("point-counter") as HTMLButtonElement;
  const counterOptions = document.getElementById("point-counter-options")!;
  const counterButtons = Array.from(
    counterOptions.querySelectorAll<HTMLButtonElement>("[data-counter]"),
  );
  const helpButton = document.getElementById("counter-help-button")!;
  const helpText = document.getElementById("counter-help-text")!;
  const help = helpButton.parentElement!;
  let counterDigits = "";
  let counterTypedAt = 0;
  const positionPopup = (popup: HTMLElement, anchor: HTMLElement) => {
    const rect = anchor.getBoundingClientRect();
    popup.style.left = `${Math.max(12, Math.min(rect.right - popup.offsetWidth, window.innerWidth - popup.offsetWidth - 12))}px`;
    popup.style.top = `${Math.max(12, rect.top - popup.offsetHeight)}px`;
  };
  let helpPinned = false;
  const showHelp = (show: boolean) => {
    helpText.hidden = !show;
    if (show) {
      helpText.showPopover();
      positionPopup(helpText, helpButton);
    } else helpText.hidePopover();
    helpButton.setAttribute("aria-expanded", String(show));
    if (!show) helpPinned = false;
  };
  help.addEventListener("mouseenter", () => showHelp(true));
  help.addEventListener("mouseleave", () => {
    if (!helpPinned && document.activeElement !== helpButton) showHelp(false);
  });
  helpButton.addEventListener("focus", () => showHelp(true));
  helpButton.addEventListener("blur", () => {
    if (!helpPinned) showHelp(false);
  });
  helpButton.addEventListener("click", () => {
    const pin = !helpPinned;
    showHelp(pin);
    helpPinned = pin;
  });
  const closeCounter = (restoreFocus = false) => {
    counterOptions.hidePopover();
    counterOptions.hidden = true;
    counter.setAttribute("aria-expanded", "false");
    if (restoreFocus) counter.focus();
  };
  const setCounter = (value: number) => {
    counter.value = String(value);
    document.getElementById("point-counter-value")!.textContent = counter.value;
    for (const button of counterButtons) {
      button.setAttribute(
        "aria-selected",
        String(button.dataset.counter === counter.value),
      );
    }
  };
  const openCounter = () => {
    counterDigits = "";
    showHelp(false);
    counterOptions.hidden = false;
    counterOptions.showPopover();
    positionPopup(counterOptions, counter);
    counter.setAttribute("aria-expanded", "true");
    const selected =
      counterButtons.find(
        (button) => button.dataset.counter === counter.value,
      ) ?? counterButtons[0];
    selected.focus({ preventScroll: true });
    selected.scrollIntoView({ block: "nearest" });
  };
  counter.addEventListener("click", () => {
    if (counterOptions.hidden) openCounter();
    else closeCounter();
  });
  counter.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      openCounter();
    }
  });
  for (const button of counterButtons) {
    button.addEventListener("click", () => {
      setCounter(Number(button.dataset.counter));
      closeCounter(true);
      onPointsChange("counter");
    });
  }
  counterOptions.addEventListener("keydown", (event) => {
    if (event.key === "Tab" && event.shiftKey) {
      event.preventDefault();
      closeCounter(true);
      return;
    }
    const index = counterButtons.indexOf(
      document.activeElement as HTMLButtonElement,
    );
    let next = index;
    if (event.key === "ArrowDown") next = (index + 1) % counterButtons.length;
    else if (event.key === "ArrowUp")
      next = (index + counterButtons.length - 1) % counterButtons.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = counterButtons.length - 1;
    else if (/^[0-9]$/.test(event.key)) {
      const now = Date.now();
      counterDigits =
        now - counterTypedAt < 700 && counterDigits.length === 1
          ? counterDigits + event.key
          : event.key;
      counterTypedAt = now;
      next = Number(counterDigits);
    } else return;
    if (!/^[0-9]$/.test(event.key)) counterDigits = "";
    event.preventDefault();
    counterButtons[next].focus({ preventScroll: true });
    counterButtons[next].scrollIntoView({ block: "nearest" });
  });
  counterOptions.addEventListener("focusout", (event) => {
    if (
      event.relatedTarget !== counter &&
      !counterOptions.contains(event.relatedTarget as Node | null)
    )
      closeCounter();
  });
  counter.addEventListener("blur", (event) => {
    if (!counterOptions.contains(event.relatedTarget as Node | null))
      closeCounter();
  });
  document.addEventListener("pointerdown", (event) => {
    const target = event.target as Node;
    if (!counter.parentElement!.contains(target)) closeCounter();
    if (!help.contains(target)) showHelp(false);
  });
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
    if (name === "points" && !open) {
      closeCounter();
      showHelp(false);
    }
    if (name === "points") onPointsChange();
  };
  const close = () => {
    if (!counterOptions.hidden) {
      closeCounter(true);
      return true;
    }
    if (!helpText.hidden) {
      showHelp(false);
      return true;
    }
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
      setCounter(settings.counter);
      refreshPoints();
    },
    pointSettings: () => ({
      ...point,
      labels: (document.getElementById("point-labels") as HTMLInputElement)
        .checked,
      allSlices: (
        document.getElementById("point-all-slices") as HTMLInputElement
      ).checked,
      counter: Number(counter.value),
    }),
  };
}
