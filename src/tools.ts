import { icon } from "./icons";
import { colorPanelMarkup } from "./color-panel";
import type { AnnotationSettings } from "./annotations";
import {
  defaultPointSettings,
  pointChoices,
  pointSymbol,
  type PointSettings,
} from "./point-settings";
const closeButton = `<button class="tool panel-close" type="button" aria-label="Close tool options" title="Close (Escape)">${icon("cancel")}</button>`;

export const toolPanels = `
  <aside id="tool-panels" aria-label="Tool options" hidden>
    ${colorPanelMarkup}
    <section id="points-panel" class="tool-panel" aria-labelledby="points-title" hidden>
      <header class="panel-heading">
        <div class="panel-title"><h2 id="points-title">Multi-point</h2>
          <div class="tool-help">
            <button id="points-help-button" class="tool-help-button" type="button" aria-label="About the multi-point tool" aria-describedby="points-help-text" aria-controls="points-help-text" aria-expanded="false">${icon("help")}</button>
            <div id="points-help-text" class="tool-tooltip" role="tooltip" popover="manual" hidden>
              <strong>Mark and edit points</strong>
              <p>Type, Color and Size change marker appearance only. They never change image pixels.</p>
              <p>Counter groups points into categories from 0 to 99, with category colors matching ImageJ. Label points optionally shows their labels.</p>
              <ul>
                <li>Press the left mouse button or E to add a point at the cursor. E adds one point per press.</li>
                <li>Hover over a point, then drag to move it.</li>
                <li>Hold Alt over a point for the minus cursor; Alt-click to remove it.</li>
                <li>Hold Space or Ctrl to pan temporarily.</li>
                <li>Use WASD or the arrow keys to navigate.</li>
              </ul>
            </div>
          </div>
        </div>${closeButton}
      </header>
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
          <div class="tool-help">
            <button id="counter-help-button" class="tool-help-button" type="button" aria-label="About counters" aria-describedby="counter-help-text" aria-controls="counter-help-text" aria-expanded="false">${icon("help")}</button>
            <div id="counter-help-text" class="tool-tooltip" role="tooltip" popover="manual" hidden>
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
  onPointsChange: (key?: keyof AnnotationSettings) => void = () => {},
  color?: { visibility: (open: boolean) => void; handleEscape: () => boolean },
) {
  const container = document.querySelector<HTMLElement>("#tool-panels")!;
  const counter = document.getElementById("point-counter") as HTMLButtonElement;
  const counterOptions = document.getElementById("point-counter-options")!;
  const counterButtons = Array.from(
    counterOptions.querySelectorAll<HTMLButtonElement>("[data-counter]"),
  );
  let counterDigits = "";
  let counterTypedAt = 0;
  const positionPopup = (popup: HTMLElement, anchor: HTMLElement) => {
    const rect = anchor.getBoundingClientRect();
    popup.style.left = `${Math.max(12, Math.min(rect.right - popup.offsetWidth, window.innerWidth - popup.offsetWidth - 12))}px`;
    popup.style.top = `${Math.max(12, rect.top - popup.offsetHeight)}px`;
  };
  const helps = ["counter", "points"].map((name) => {
    const button = document.getElementById(`${name}-help-button`)!;
    const text = document.getElementById(`${name}-help-text`)!;
    const wrapper = button.parentElement!;
    let pinned = false;
    const show = (visible: boolean) => {
      text.hidden = !visible;
      if (visible) {
        closeCounter();
        for (const other of helps) if (other.text !== text) other.show(false);
        const below =
          name === "points"
            ? button.closest("header")!.getBoundingClientRect().bottom
            : null;
        if (below !== null)
          text.style.maxHeight = `${window.innerHeight - below - 12}px`;
        text.showPopover();
        positionPopup(text, button);
        if (below !== null) text.style.top = `${below}px`;
      } else text.hidePopover();
      button.setAttribute("aria-expanded", String(visible));
      if (!visible) pinned = false;
    };
    wrapper.addEventListener("mouseenter", () => show(true));
    wrapper.addEventListener("mouseleave", () => {
      if (!pinned && document.activeElement !== button) show(false);
    });
    button.addEventListener("focus", () => show(true));
    button.addEventListener("blur", () => {
      if (!pinned) show(false);
    });
    button.addEventListener("click", () => {
      const pin = !pinned;
      show(pin);
      pinned = pin;
    });
    return { text, wrapper, show };
  });
  const hideHelp = () => helps.forEach((help) => help.show(false));
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
    hideHelp();
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
    for (const help of helps)
      if (!help.wrapper.contains(target)) help.show(false);
  });
  const names = ["color", "points"] as const;
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
    if (name === "color") color?.visibility(open);
    if (name === "points" && !open) {
      closeCounter();
      hideHelp();
    }
    if (name === "points") onPointsChange();
  };
  const close = () => {
    if (color?.handleEscape()) return true;
    if (!counterOptions.hidden) {
      closeCounter(true);
      return true;
    }
    if (helps.some((help) => !help.text.hidden)) {
      hideHelp();
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
