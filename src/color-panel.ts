import { icon } from "./icons";
import {
  adjustColorConnection,
  colorName,
  colorConnectionValue,
  connectColor,
  copyColorSettings,
  defaultColorSettings,
  removeColorConnection,
  resetColorConnection,
  setOutputColor,
  setColorConnectionRange,
  toHex,
  toHSV,
  type ColorAdjustment,
  type ColorChannel,
  type ColorSettings,
} from "./color-settings";
import { percentileRange } from "./display-settings";
import "./color-panel.css";

const trash = icon("trash");
export const colorPanelMarkup = `
  <section id="color-panel" class="tool-panel" aria-labelledby="color-panel-title" hidden>
    <header class="panel-heading">
      <div class="panel-title"><h2 id="color-panel-title">Color</h2><div class="tool-help">
        <button id="color-help-button" class="tool-help-button" type="button" aria-label="About color connections" aria-expanded="false" aria-controls="color-help-text" aria-describedby="color-help-text">${icon("help")}</button>
        <div id="color-help-text" class="tool-tooltip" role="tooltip" popover="manual" hidden>
          <strong>Color connections</strong><ul>
            <li>Drag a channel to a color, or click them in order. Channels and colors can have several connections.</li>
            <li>Click a color to change its shade or hex value.</li>
            <li>Click a wire to adjust its range and brightness using the source channel histogram. Brightness scales the full ramp and plateau. These adjustments change the display only.</li>
            <li>Drag a wire to another color to move it. Drop in empty space to remove it. Escape cancels.</li>
            <li>The trash button removes the selected wire.</li>
            <li>With a wire focused, Enter selects, R starts moving, and Delete removes. Then choose a color with Tab and Enter.</li>
          </ul>
        </div>
      </div></div>
      <div class="color-heading-actions"><button id="reset-colors" class="tool" type="button" aria-label="Reset color connections" title="Reset color connections">${icon("reset")}</button><button class="tool panel-close" type="button" aria-label="Close color options" title="Close (Escape)">${icon("cancel")}</button></div>
    </header>
    <div id="color-patch" class="color-patch"><svg id="color-wires" class="color-wires" aria-label="Channel connections"></svg><div id="color-inputs" class="color-inputs"></div><div id="color-outputs" class="color-outputs"></div></div>
    <div id="color-inspector" class="color-inspector" hidden></div>
    <div id="color-status" class="color-status" role="status" aria-live="polite"></div>
    <div id="color-editor" popover="auto" role="dialog" aria-label="Output color">
      <div class="color-editor-heading"><span id="color-editor-title">Color</span><button id="close-color-editor" class="tool" type="button" aria-label="Close color picker">${icon("cancel")}</button></div>
      <div id="color-shade" role="slider" tabindex="0" aria-label="Saturation and brightness" aria-valuemin="0" aria-valuemax="100" aria-valuenow="100" aria-valuetext="100% saturation, 100% brightness"><span id="color-shade-dot"></span></div>
      <input id="color-hue" type="range" min="0" max="359" value="0" aria-label="Hue">
      <label class="color-hex-row" for="color-hex">Hex<input id="color-hex" spellcheck="false" autocomplete="off" maxlength="7" aria-describedby="color-hex-error"></label><p id="color-hex-error" hidden>Use 3 or 6 hex digits.</p>
    </div>
  </section>`;

const escape = (value: string) =>
  value.replace(
    /[&<>"']/g,
    (char) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        char
      ]!,
  );
const curve = (a: { x: number; y: number }, b: { x: number; y: number }) => {
  const bend = Math.max(35, Math.abs(b.x - a.x) * 0.5);
  return `M${a.x},${a.y} C${a.x + bend},${a.y} ${b.x - bend},${b.y} ${b.x},${b.y}`;
};

// Keep pale neutral connection graphics legible without changing image colors.
const portColor = (hex: string) => {
  const { s, v } = toHSV(hex);
  return s < 0.12 && v > 0.9 ? `var(--color-neutral-port, ${hex})` : hex;
};

export function bindColorPanel(onChange: (settings: ColorSettings) => void) {
  const panel = document.getElementById("color-panel")!;
  const get = <T extends HTMLElement = HTMLElement>(id: string) =>
    document.getElementById(`color-${id}`) as T;
  const patch = get("patch"),
    wires = document.getElementById("color-wires")!;
  const editor = get("editor"),
    shade = get("shade"),
    hex = get<HTMLInputElement>("hex"),
    hue = get<HTMLInputElement>("hue");
  const help = get("help-button"),
    helpText = get("help-text"),
    helpWrap = help.parentElement!;
  let channels: ColorChannel[] = [],
    settings = defaultColorSettings([]),
    enabled = false,
    selected: number | null = null;
  let histograms: readonly (Uint32Array | null)[] = [],
    logarithmic = false;
  let editing: string | null = null,
    hsv = { h: 0, s: 1, v: 1 },
    helpPinned = false,
    suppressClick = false;
  type Pending = { source: string; edgeId?: number };
  let pending: Pending | null = null;
  let gesture:
    | (Pending & {
        startX: number;
        startY: number;
        moved: boolean;
        pointerId: number;
      })
    | null = null;
  const edgeById = (id: number | null) =>
    settings.connections.find((edge) => edge.id === id);
  const sourceButton = (source: string) =>
    patch.querySelector<HTMLButtonElement>(
      `[data-source="${CSS.escape(source)}"]`,
    )!;
  const outputButton = (color: string) =>
    patch.querySelector<HTMLButtonElement>(
      `[data-color="${CSS.escape(color)}"]`,
    )!;
  const announce = (message: string) => {
    get("status").textContent = message;
  };
  const label = (edge: NonNullable<ReturnType<typeof edgeById>>) =>
    `${channels.find((channel) => channel.id === edge.source)?.name ?? edge.source} → ${colorName(settings.colors[edge.color])}`;
  const publish = () => onChange(copyColorSettings(settings));
  const focusEdge = (id: number) =>
    patch
      .querySelector<SVGElement>(`[data-edge="${id}"]`)
      ?.focus({ preventScroll: true });
  const point = (element: Element) => {
    const rect = element.getBoundingClientRect(),
      board = patch.getBoundingClientRect();
    return {
      x: rect.x - board.x + rect.width / 2,
      y: rect.y - board.y + rect.height / 2,
    };
  };
  function draw() {
    const focus = document.activeElement?.getAttribute("data-edge");
    wires.innerHTML = [...settings.connections]
      .sort((a, b) => Number(a.id === selected) - Number(b.id === selected))
      .filter((edge) => !(gesture?.moved && gesture.edgeId === edge.id))
      .map((edge) => {
        const from = sourceButton(edge.source)?.querySelector(".color-dot"),
          to = outputButton(edge.color)?.querySelector(".color-dot");
        if (!from || !to) return "";
        const d = curve(point(from), point(to));
        return `<g class="color-connection ${edge.id === selected ? "selected" : ""}" style="--wire-color:${portColor(settings.colors[edge.color])}"><path class="color-wire" d="${d}" stroke="${portColor(settings.colors[edge.color])}"/><path class="color-wire-hit" data-edge="${edge.id}" d="${d}" tabindex="${enabled ? 0 : -1}" role="button" aria-pressed="${edge.id === selected}" aria-label="${escape(label(edge))} connection"/></g>`;
      })
      .join("");
    if (focus) focusEdge(Number(focus));
  }
  const selectedHistogram = () => {
    const edge = edgeById(selected);
    return edge
      ? (histograms[
          channels.findIndex((channel) => channel.id === edge.source)
        ] ?? null)
      : null;
  };
  function refreshInspectorRange() {
    const edge = edgeById(selected);
    if (!edge) return;
    for (const field of ["minimum", "maximum", "brightness"] as const) {
      const input = get<HTMLInputElement>(`range-${field}`);
      if (!input) continue;
      input.value = String(edge[field]);
      input.style.setProperty(
        "--fill",
        `${((edge[field] - Number(input.min)) / (Number(input.max) - Number(input.min))) * 100}%`,
      );
      get(`value-${field}`).textContent =
        `${edge[field]}${field === "brightness" ? "%" : ""}`;
      input.setAttribute(
        "aria-valuetext",
        `${edge[field]}${field === "brightness" ? "%" : ""}`,
      );
      if (field === "brightness") continue;
      const cutoff = document.getElementById(`color-cutoff-${field}`)!;
      cutoff.setAttribute("x1", String(edge[field]));
      cutoff.setAttribute("x2", String(edge[field]));
    }
    document
      .getElementById("color-range-line")!
      .setAttribute(
        "d",
        Array.from(
          { length: 256 },
          (_, x) =>
            `${x === 0 ? "M" : "L"}${x} ${64 * (1 - colorConnectionValue(x, edge))}`,
        ).join(""),
      );
  }
  function refreshInspectorHistogram() {
    if (!edgeById(selected)) return;
    const bins = selectedHistogram(),
      peak = bins ? Math.max(...bins) : 0;
    document
      .getElementById("color-histogram-bars")!
      .setAttribute(
        "d",
        bins && peak > 0
          ? `M0 64${Array.from(bins, (count, x) => `L${x} ${64 - 62 * (logarithmic ? Math.log1p(count) / Math.log1p(peak) : count / peak)}`).join("")}L255 64Z`
          : "",
      );
    get("inspector")
      .querySelectorAll<HTMLButtonElement>("[data-color-percentile]")
      .forEach((button) => (button.disabled = !enabled || !peak));
    get("histogram-description").textContent = bins
      ? `Approximate original source channel histogram from an 8-bit preview sample; ${logarithmic ? "logarithmic" : "linear"} counts`
      : "Source channel histogram unavailable";
    const scale = get<HTMLButtonElement>("histogram-scale");
    scale.textContent = logarithmic ? "Log" : "Linear";
    scale.setAttribute("aria-pressed", String(logarithmic));
    scale.title = `Switch to ${logarithmic ? "linear" : "logarithmic"} scale`;
  }
  function renderInspector() {
    const edge = edgeById(selected),
      inspector = get("inspector");
    inspector.hidden = !edge;
    if (!edge) {
      inspector.innerHTML = "";
      return;
    }
    const source =
        channels.find((channel) => channel.id === edge.source)?.name ??
        edge.source,
      color = colorName(settings.colors[edge.color]);
    inspector.innerHTML = `<div class="color-inspector-heading">
      <span class="color-connection-source" title="${escape(source)}">${escape(source)}</span>
      <svg class="color-connection-arrow" viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14m-7-7 7 7-7 7"/></svg>
      <span class="color-connection-output" title="${escape(color)}"><span class="color-swatch" style="background:${settings.colors[edge.color]}"></span><span>${escape(color)}</span></span>
      <button id="delete-color-connection" class="tool" type="button" aria-label="Delete ${escape(label(edge))} connection" title="Delete connection" ${enabled ? "" : "disabled"}>${trash}</button>
    </div>
    <div class="histogram-container">
      <svg class="range-graph" viewBox="0 0 255 64" preserveAspectRatio="none" role="img" aria-label="Approximate source channel histogram">
        <title id="color-histogram-description"></title>
        <path class="graph-grid" d="M63.75 0v64M127.5 0v64M191.25 0v64M0 32h255"/>
        <path id="color-histogram-bars"/>
        <path id="color-range-line"/>
        <line id="color-cutoff-minimum" class="histogram-cutoff" y1="0" y2="64"/>
        <line id="color-cutoff-maximum" class="histogram-cutoff" y1="0" y2="64"/>
      </svg>
      <button id="color-histogram-scale" class="text-button histogram-scale" type="button" aria-label="Logarithmic histogram scale" ${enabled ? "" : "disabled"}></button>
    </div>
    <div class="range-labels"><span>0</span><span>255</span></div>
    ${(["minimum", "maximum"] as const).map((field) => `<label class="adjustment" for="color-range-${field}"><span>${field === "minimum" ? "Minimum" : "Maximum"}</span><input id="color-range-${field}" type="range" data-color-field="${field}" min="${field === "minimum" ? 0 : 1}" max="${field === "minimum" ? 254 : 255}" aria-label="${escape(label(edge))} ${field}" ${enabled ? "" : "disabled"}><output id="color-value-${field}" for="color-range-${field}"></output></label>`).join("")}
    <label class="adjustment" for="color-range-brightness"><span>Brightness</span><input id="color-range-brightness" type="range" data-color-field="brightness" min="0" max="100" step="1" aria-label="${escape(label(edge))} brightness" ${enabled ? "" : "disabled"}><output id="color-value-brightness" for="color-range-brightness"></output></label>
    <footer class="panel-footer display-presets">${[0, 0.1, 0.5, 1].map((tail) => `<button class="text-button" type="button" data-color-percentile="${tail}" title="${tail === 0 ? "Set display range to the sampled source channel minimum and maximum" : `Set display range to the ${tail}–${100 - tail} percentiles`}">${tail}–${100 - tail}</button>`).join("")}<button id="color-reset-range" class="text-button" type="button" ${enabled ? "" : "disabled"}>${icon("reset")}Reset</button></footer>`;
    refreshInspectorRange();
    refreshInspectorHistogram();
  }
  function renderOutputs() {
    const current = edgeById(selected);
    get("outputs").innerHTML = Object.entries(settings.colors)
      .map(([color, value]) => {
        const name = colorName(value);
        return `<button class="color-terminal ${settings.connections.some((edge) => edge.color === color) ? "connected" : ""} ${current?.color === color ? "selected" : ""}" type="button" data-color="${escape(color)}" style="--port-color:${portColor(value)}" aria-label="${name}, ${value}. Connect or edit color" aria-haspopup="dialog" ${enabled ? "" : "disabled"}><span class="color-dot" aria-hidden="true"></span><span class="color-label"><span>${name}</span>${name.endsWith("-ish") ? `<small>${value.toUpperCase()}</small>` : ""}</span></button>`;
      })
      .join("");
  }
  function render() {
    const current = edgeById(selected);
    get("inputs").innerHTML = channels
      .map(
        (channel) =>
          `<button class="color-terminal ${settings.connections.some((edge) => edge.source === channel.id) ? "connected" : ""} ${current?.source === channel.id ? "selected" : ""} ${pending?.source === channel.id ? "pending" : ""}" type="button" data-source="${escape(channel.id)}" style="--port-color:${channel.color && /^#[a-f0-9]{6}$/i.test(channel.color) ? portColor(channel.color) : "var(--accent)"}" aria-label="Connect ${escape(channel.name)}" ${enabled ? "" : "disabled"}><span>${escape(channel.name)}</span><span class="color-dot" aria-hidden="true"></span></button>`,
      )
      .join("");
    patch.style.height = `${Math.max(210, Object.keys(settings.colors).length * 34, channels.length * 30)}px`;
    renderOutputs();
    renderInspector();
    draw();
    (document.getElementById("reset-colors") as HTMLButtonElement).disabled =
      !enabled;
  }
  function cancelConnection() {
    const pointerId = gesture?.pointerId;
    pending = null;
    gesture = null;
    if (pointerId !== undefined && patch.hasPointerCapture(pointerId))
      patch.releasePointerCapture(pointerId);
    patch
      .querySelectorAll(".pending,.drop-target")
      .forEach((element) => element.classList.remove("pending", "drop-target"));
    draw();
  }
  function showHelp(show: boolean) {
    helpText.hidden = !show;
    help.setAttribute("aria-expanded", String(show));
    if (show) {
      const rect = help.getBoundingClientRect();
      helpText.showPopover();
      helpText.style.left = `${Math.max(12, Math.min(rect.left, innerWidth - helpText.offsetWidth - 12))}px`;
      helpText.style.top = `${Math.min(rect.bottom + 8, innerHeight - helpText.offsetHeight - 12)}px`;
    } else {
      helpText.hidePopover();
      helpPinned = false;
    }
  }
  function closeEditor(restoreFocus = false) {
    editor.hidePopover();
    if (restoreFocus && editing)
      outputButton(editing)?.focus({ preventScroll: true });
    editing = null;
  }
  function closeTransient() {
    cancelConnection();
    closeEditor();
    showHelp(false);
  }
  function selectEdge(id: number) {
    selected = id;
    render();
    focusEdge(id);
  }
  function removeEdge(id: number) {
    const edge = edgeById(id);
    if (!edge) return;
    settings = removeColorConnection(settings, id);
    selected = settings.connections[0]?.id ?? null;
    cancelConnection();
    render();
    publish();
    if (selected !== null) focusEdge(selected);
    else sourceButton(edge.source)?.focus();
    announce(`Removed ${label(edge)}.`);
  }
  function beginConnection(source: string, edgeId?: number) {
    closeEditor();
    cancelConnection();
    pending = { source, edgeId };
    sourceButton(source)?.classList.add("pending");
    if (edgeId !== undefined) focusEdge(edgeId);
    announce("Choose a color. Escape cancels.");
  }
  function commit(source: string, color: string, edgeId?: number) {
    const changed = connectColor(settings, source, color, edgeId);
    selected =
      edgeId ??
      changed.connections.find(
        (edge) => edge.source === source && edge.color === color,
      )!.id;
    if (changed === settings)
      announce(
        "That channel is already connected to this color. Existing connections kept.",
      );
    else {
      settings = changed;
      publish();
      announce(`Connected ${label(edgeById(selected)!)}.`);
    }
    cancelConnection();
    render();
    focusEdge(selected);
  }
  const targetColor = (x: number, y: number) => {
    const target = document
      .elementFromPoint(x, y)
      ?.closest<HTMLButtonElement>("[data-color]");
    return target && patch.contains(target) ? target.dataset.color : undefined;
  };
  patch.addEventListener("pointerdown", (event) => {
    suppressClick = false;
    if (!enabled || event.button !== 0) return;
    const target = event.target as Element,
      source = target.closest<HTMLElement>("[data-source]"),
      wire = target.closest<Element>("[data-edge]");
    if (!source && !wire) return;
    const edge = wire ? edgeById(Number(wire.getAttribute("data-edge"))) : null;
    cancelConnection();
    gesture = {
      source: edge?.source ?? source!.dataset.source!,
      edgeId: edge?.id,
      startX: event.clientX,
      startY: event.clientY,
      moved: false,
      pointerId: event.pointerId,
    };
    patch.setPointerCapture(event.pointerId);
    if (wire) event.preventDefault();
  });
  patch.addEventListener("pointermove", (event) => {
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    if (
      !gesture.moved &&
      Math.hypot(
        event.clientX - gesture.startX,
        event.clientY - gesture.startY,
      ) < 5
    )
      return;
    gesture.moved = true;
    if (gesture.edgeId !== undefined) {
      selected = gesture.edgeId;
      renderInspector();
    }
    draw();
    const color = targetColor(event.clientX, event.clientY),
      rect = patch.getBoundingClientRect();
    patch
      .querySelectorAll(".drop-target")
      .forEach((el) => el.classList.remove("drop-target"));
    if (color) outputButton(color).classList.add("drop-target");
    const ghost = document.createElementNS(
      "http://www.w3.org/2000/svg",
      "path",
    );
    ghost.setAttribute("class", "color-ghost");
    ghost.setAttribute(
      "d",
      curve(point(sourceButton(gesture.source).querySelector(".color-dot")!), {
        x: event.clientX - rect.x,
        y: event.clientY - rect.y,
      }),
    );
    ghost.setAttribute(
      "stroke",
      color ? portColor(settings.colors[color]) : "var(--accent)",
    );
    if (!color) ghost.setAttribute("stroke-dasharray", "4 3");
    wires.append(ghost);
  });
  patch.addEventListener("pointerup", (event) => {
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    const drag = gesture,
      color = targetColor(event.clientX, event.clientY);
    gesture = null;
    if (patch.hasPointerCapture(event.pointerId))
      patch.releasePointerCapture(event.pointerId);
    if (!drag.moved) {
      if (drag.edgeId !== undefined) selectEdge(drag.edgeId);
      else beginConnection(drag.source);
      return;
    }
    suppressClick = true;
    // Prevent the release click from editing its drop target, then expire even
    // when the browser emits no click after a captured/prevented wire drag.
    window.setTimeout(() => (suppressClick = false), 0);
    if (color) commit(drag.source, color, drag.edgeId);
    else if (drag.edgeId !== undefined) removeEdge(drag.edgeId);
    else {
      cancelConnection();
      announce("Connection cancelled.");
    }
  });
  patch.addEventListener("click", (event) => {
    if (suppressClick && event.detail !== 0) {
      suppressClick = false;
      return;
    }
    if (!enabled) return;
    const target = event.target as Element,
      source = target.closest<HTMLElement>("[data-source]"),
      wire = target.closest<Element>("[data-edge]"),
      output = target.closest<HTMLElement>("[data-color]");
    if (event.detail === 0 && source) beginConnection(source.dataset.source!);
    else if (event.detail === 0 && wire)
      selectEdge(Number(wire.getAttribute("data-edge")));
    else if (output && pending)
      commit(pending.source, output.dataset.color!, pending.edgeId);
    else if (output) openEditor(output.dataset.color!);
  });
  patch.addEventListener("keydown", (event) => {
    const wire = (event.target as Element).closest<Element>("[data-edge]");
    if (!wire || !enabled) return;
    const id = Number(wire.getAttribute("data-edge"));
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      selectEdge(id);
    } else if (event.key.toLowerCase() === "r") {
      event.preventDefault();
      const edge = edgeById(id)!;
      beginConnection(edge.source, id);
    } else if (event.key === "Delete" || event.key === "Backspace") {
      event.preventDefault();
      removeEdge(id);
    }
  });
  get("inspector").addEventListener("click", (event) => {
    if (!enabled || selected === null) return;
    const target = event.target as Element;
    if (target.closest("#delete-color-connection")) {
      removeEdge(selected);
      return;
    }
    if (target.closest("#color-histogram-scale")) {
      logarithmic = !logarithmic;
      refreshInspectorHistogram();
      return;
    }
    const preset = target.closest<HTMLButtonElement>("[data-color-percentile]");
    if (preset) {
      const bins = selectedHistogram();
      if (!bins) return;
      const range = percentileRange(
        Array.from(bins),
        Number(preset.dataset.colorPercentile),
      );
      settings = setColorConnectionRange(settings, selected, range);
    } else if (target.closest("#color-reset-range"))
      settings = resetColorConnection(settings, selected);
    else return;
    refreshInspectorRange();
    publish();
  });
  get("inspector").addEventListener("input", (event) => {
    const input = event.target as HTMLInputElement,
      field = input.dataset.colorField as keyof ColorAdjustment | undefined;
    if (!enabled || !field || selected === null) return;
    settings = adjustColorConnection(
      settings,
      selected,
      field,
      input.valueAsNumber,
    );
    refreshInspectorRange();
    publish();
  });
  document.getElementById("reset-colors")!.addEventListener("click", () => {
    if (!enabled) return;
    closeTransient();
    settings = defaultColorSettings(channels);
    selected = settings.connections[0]?.id ?? null;
    render();
    publish();
    announce("Color connections reset.");
  });
  function syncPicker(syncHex = true) {
    if (!editing) return;
    shade.style.setProperty("--hue", `hsl(${hsv.h} 100% 50%)`);
    get("shade-dot").style.left = `${hsv.s * 100}%`;
    get("shade-dot").style.top = `${(1 - hsv.v) * 100}%`;
    shade.setAttribute("aria-valuenow", String(Math.round(hsv.s * 100)));
    shade.setAttribute(
      "aria-valuetext",
      `${Math.round(hsv.s * 100)}% saturation, ${Math.round(hsv.v * 100)}% brightness`,
    );
    hue.value = String(hsv.h);
    if (syncHex) hex.value = settings.colors[editing].toUpperCase();
    get("editor-title").textContent = colorName(settings.colors[editing]);
  }
  function updateColor(value: string, syncHex = true) {
    if (!editing) return;
    settings = setOutputColor(settings, editing, value);
    renderOutputs();
    renderInspector();
    draw();
    syncPicker(syncHex);
    publish();
    hex.removeAttribute("aria-invalid");
    get("hex-error").hidden = true;
  }
  function openEditor(color: string) {
    showHelp(false);
    editing = color;
    hsv = toHSV(settings.colors[color]);
    syncPicker();
    hex.removeAttribute("aria-invalid");
    get("hex-error").hidden = true;
    editor.showPopover();
    const rect = outputButton(color).getBoundingClientRect();
    editor.style.left = `${Math.max(10, Math.min(innerWidth - editor.offsetWidth - 10, rect.right - editor.offsetWidth))}px`;
    editor.style.top = `${Math.max(10, Math.min(innerHeight - editor.offsetHeight - 10, rect.bottom + 5))}px`;
    shade.focus({ preventScroll: true });
  }
  document
    .getElementById("close-color-editor")!
    .addEventListener("click", () => closeEditor(true));
  hue.addEventListener("input", () => {
    hsv.h = hue.valueAsNumber;
    updateColor(toHex(hsv));
  });
  function setShade(event: PointerEvent) {
    const rect = shade.getBoundingClientRect();
    hsv.s = Math.max(0, Math.min(1, (event.clientX - rect.x) / rect.width));
    hsv.v =
      1 - Math.max(0, Math.min(1, (event.clientY - rect.y) / rect.height));
    updateColor(toHex(hsv));
  }
  shade.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    shade.focus();
    shade.setPointerCapture(event.pointerId);
    setShade(event);
  });
  shade.addEventListener("pointermove", (event) => {
    if (shade.hasPointerCapture(event.pointerId) && event.buttons === 1)
      setShade(event);
  });
  shade.addEventListener("keydown", (event) => {
    if (
      !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)
    )
      return;
    event.preventDefault();
    hsv.s = Math.max(
      0,
      Math.min(
        1,
        hsv.s +
          (event.key === "ArrowRight"
            ? 0.01
            : event.key === "ArrowLeft"
              ? -0.01
              : 0),
      ),
    );
    hsv.v = Math.max(
      0,
      Math.min(
        1,
        hsv.v +
          (event.key === "ArrowUp"
            ? 0.01
            : event.key === "ArrowDown"
              ? -0.01
              : 0),
      ),
    );
    updateColor(toHex(hsv));
  });
  hex.addEventListener("input", () => {
    if (!editing) return;
    const next = setOutputColor(settings, editing, hex.value);
    if (next === settings) {
      hex.setAttribute("aria-invalid", "true");
      get("hex-error").hidden = false;
      return;
    }
    hsv = toHSV(next.colors[editing]);
    updateColor(hex.value, false);
  });
  const handleEscape = () => {
    if (editor.matches(":popover-open")) {
      closeEditor(true);
      return true;
    }
    if (pending || gesture) {
      cancelConnection();
      announce("Connection cancelled.");
      return true;
    }
    if (!helpText.hidden) {
      showHelp(false);
      return true;
    }
    return false;
  };
  editor.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeEditor(true);
    }
  });
  editor.addEventListener("toggle", () => {
    if (!editor.matches(":popover-open")) editing = null;
  });
  help.addEventListener("click", () => {
    const pinned = !helpPinned;
    showHelp(pinned);
    helpPinned = pinned;
  });
  helpWrap.addEventListener("pointerenter", () => showHelp(true));
  helpWrap.addEventListener("pointerleave", () => {
    if (!helpPinned && !helpWrap.contains(document.activeElement))
      showHelp(false);
  });
  help.addEventListener("focus", () => showHelp(true));
  help.addEventListener("blur", () => {
    if (!helpPinned) showHelp(false);
  });
  document.addEventListener("pointerdown", (event) => {
    if (!helpWrap.contains(event.target as Node)) showHelp(false);
  });
  patch.addEventListener("pointercancel", cancelConnection);
  patch.addEventListener("lostpointercapture", () => {
    if (gesture) cancelConnection();
  });
  window.addEventListener("blur", closeTransient);
  window.addEventListener("resize", () => {
    closeTransient();
    draw();
  });
  new ResizeObserver(() => draw()).observe(patch);
  render();
  return {
    setChannels(next: readonly ColorChannel[]) {
      closeTransient();
      channels = next.map((channel) => ({ ...channel }));
      histograms = [];
      settings = defaultColorSettings(channels);
      selected = settings.connections[0]?.id ?? null;
      render();
      publish();
    },
    setHistograms(next: readonly (Uint32Array | null)[]) {
      histograms = next;
      refreshInspectorHistogram();
    },
    settings: () => copyColorSettings(settings),
    setEnabled(ready: boolean) {
      enabled = ready;
      if (!ready) closeTransient();
      render();
    },
    visibility(open: boolean) {
      if (!open) closeTransient();
      else requestAnimationFrame(draw);
    },
    closeTransient,
    handleEscape,
    redraw: draw,
  };
}
