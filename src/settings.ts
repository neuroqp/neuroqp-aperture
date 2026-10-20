import { invoke, isTauri } from "@tauri-apps/api/core";
import { icon } from "./icons";
import "./settings.css";

export const settingsPanel = `
  <dialog id="settings-dialog" aria-labelledby="settings-label settings-title">
    <header class="settings-titlebar">
      <span id="settings-label" data-tauri-drag-region>Settings</span>
      <div class="window-actions">
        <button id="settings-minimize-button" class="window-button" type="button" title="Minimize" aria-label="Minimize">${icon("minimize")}</button>
        <button id="settings-maximize-button" class="window-button" type="button" title="Maximize" aria-label="Maximize">${icon("maximize")}</button>
        <button id="settings-window-close-button" class="window-button close" type="button" title="Close" aria-label="Close">${icon("cancel")}</button>
      </div>
    </header>
    <aside class="settings-sidebar">
      <nav aria-label="Settings">
        <button id="settings-general-button" class="settings-navigation" type="button" aria-current="page" aria-controls="settings-general" autofocus>${icon("general")}<span>General</span></button>
        <button id="settings-appearance-button" class="settings-navigation" type="button" aria-controls="settings-appearance">${icon("color")}<span>Appearance</span></button>
      </nav>
    </aside>
    <main class="settings-content">
      <header class="settings-heading">
        <h1 id="settings-title">General</h1>
        <button id="settings-close" class="tool" type="button" title="Close settings" aria-label="Close settings">${icon("cancel")}</button>
      </header>
      <section id="settings-general" aria-labelledby="update-title">
        <h2>Image memory</h2>
        <form id="memory-form" class="settings-field memory-settings">
          <div>
            <label for="memory-limit">Image cache and decoding budget across all windows</label>
            <p id="memory-description">Covers image caches and decoding. App overhead and GPU memory are separate. More memory keeps more planes ready. Image quality stays the same.</p>
            <p id="memory-usage">Default limit: 4 GiB.</p>
            <p id="memory-error" role="alert" hidden></p>
          </div>
          <div class="memory-controls">
            <label class="memory-value"><input id="memory-limit" type="number" min="0.25" max="256" step="any" value="4" required aria-describedby="memory-description memory-usage memory-error"> GiB</label>
            <button id="memory-apply" class="file-button" type="submit">Apply</button>
          </div>
        </form>
        <h2 id="update-title">Updates</h2>
        <div class="settings-update-overview settings-field">
          <div>
            <p id="update-edition"></p>
            <p id="update-status" role="status" aria-live="polite">Check for a new version.</p>
          </div>
          <button id="update-check" class="file-button" type="button">Check for updates</button>
        </div>
        <pre id="update-notes" hidden></pre>
        <progress id="update-download-progress" aria-label="Update download" hidden></progress>
        <div class="settings-auto settings-field">
          <div>
            <label for="update-auto">Automatic updates</label>
            <p id="update-auto-description">Check for updates once a day. Downloads and restarts need your confirmation.</p>
          </div>
          <input id="update-auto" type="checkbox" aria-describedby="update-auto-description">
        </div>
        <div class="settings-update-actions">
          <button id="update-action" class="file-button save-primary" type="button" hidden>Download update</button>
          <button id="update-later" class="file-button" type="button" hidden>Later</button>
          <button id="update-releases" class="file-button" type="button">View releases</button>
        </div>
      </section>
      <section id="settings-appearance" aria-labelledby="theme-label" hidden>
        <div class="settings-field">
          <div>
            <label id="theme-label" for="theme-select">Theme</label>
            <p id="theme-description">Choose an appearance, or follow your system.</p>
          </div>
          <select id="theme-select" aria-describedby="theme-description">
            <option value="system">System</option>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </div>
      </section>
    </main>
  </dialog>`;

export function bindSettings() {
  const dialog = document.getElementById(
    "settings-dialog",
  ) as HTMLDialogElement;
  const trigger = document.getElementById(
    "settings-button",
  ) as HTMLButtonElement;
  const closeButton = document.getElementById(
    "settings-close",
  ) as HTMLButtonElement;
  const general = document.getElementById("settings-general-button")!;
  const appearance = document.getElementById("settings-appearance-button")!;
  let busy = false;
  const memoryLimit = document.getElementById(
    "memory-limit",
  ) as HTMLInputElement;
  const memoryError = document.getElementById("memory-error")!;
  const memoryUsage = document.getElementById("memory-usage")!;
  const memoryApply = document.getElementById(
    "memory-apply",
  ) as HTMLButtonElement;
  let memoryTimer: ReturnType<typeof setTimeout> | undefined;
  let memorySaving = false;
  let memoryEdited = false;
  let memoryGeneration = 0;
  memoryLimit.addEventListener("input", () => {
    memoryEdited = true;
  });
  type MemoryStatus = {
    limitBytes: number;
    usedBytes: number;
    pressure: boolean;
    preloadingPaused: boolean;
  };
  const renderMemory = (status: MemoryStatus) => {
    if (
      !memoryEdited &&
      document.activeElement !== memoryLimit &&
      !memorySaving
    )
      memoryLimit.value = String(status.limitBytes / 1024 ** 3);
    const used = (status.usedBytes / 1024 ** 3).toLocaleString(undefined, {
      maximumFractionDigits: 2,
    });
    memoryUsage.textContent = `${used} GiB reserved across Aperture windows${status.preloadingPaused ? " · Preloading paused" : status.pressure ? " · Memory pressure" : ""}`;
  };
  const refreshMemory = async () => {
    clearTimeout(memoryTimer);
    if (!isTauri() || !dialog.open) return;
    const generation = ++memoryGeneration;
    try {
      const status = await invoke<MemoryStatus>("memory_status");
      if (!memorySaving && generation === memoryGeneration)
        renderMemory(status);
    } catch (error) {
      if (generation !== memoryGeneration) return;
      memoryError.textContent = `Memory usage unavailable: ${String(error)}`;
      memoryError.hidden = false;
    }
    if (dialog.open) memoryTimer = setTimeout(() => void refreshMemory(), 2000);
  };
  document
    .getElementById("memory-form")!
    .addEventListener("submit", (event) => {
      event.preventDefault();
      if (!memoryLimit.reportValidity() || memorySaving || !isTauri()) return;
      const limitBytes = Math.round(Number(memoryLimit.value) * 1024 ** 3);
      if (!Number.isSafeInteger(limitBytes)) return;
      memorySaving = true;
      memoryGeneration++;
      memoryApply.disabled = true;
      memoryError.hidden = true;
      void invoke<MemoryStatus>("set_memory_budget", { limitBytes })
        .then(
          (status) => {
            memorySaving = false;
            memoryEdited = false;
            memoryLimit.value = String(status.limitBytes / 1024 ** 3);
            renderMemory(status);
          },
          (error) => {
            memoryError.textContent = `Could not save memory limit: ${String(error)}`;
            memoryError.hidden = false;
          },
        )
        .finally(() => {
          memorySaving = false;
          memoryApply.disabled = false;
        });
    });

  const select = (section: "general" | "appearance") => {
    for (const name of ["general", "appearance"]) {
      const selected = name === section;
      document.getElementById(`settings-${name}`)!.hidden = !selected;
      const button = document.getElementById(`settings-${name}-button`)!;
      if (selected) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
    document.getElementById("settings-title")!.textContent =
      section === "general" ? "General" : "Appearance";
  };
  const showGeneral = () => {
    select("general");
    if (!dialog.open) {
      memoryEdited = false;
      dialog.showModal();
    }
    trigger.setAttribute("aria-expanded", "true");
    void refreshMemory();
  };
  const close = () => dialog.close();
  trigger.addEventListener("click", showGeneral);
  general.addEventListener("click", () => select("general"));
  appearance.addEventListener("click", () => select("appearance"));
  closeButton.addEventListener("click", () => {
    if (!busy) close();
  });
  dialog.addEventListener("cancel", (event) => {
    if (busy) event.preventDefault();
  });
  dialog.addEventListener("close", () => {
    if (dialog.open) return;
    clearTimeout(memoryTimer);
    trigger.setAttribute("aria-expanded", "false");
    if (!document.querySelector("dialog[open]")) trigger.focus();
  });

  return {
    showGeneral,
    close,
    busy: () => busy,
    setBusy(value: boolean) {
      busy = value;
      closeButton.disabled = value;
      (
        document.getElementById(
          "settings-window-close-button",
        ) as HTMLButtonElement
      ).disabled = value;
    },
  };
}
