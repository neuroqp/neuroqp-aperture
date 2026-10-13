import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  announceUpdate,
  automaticCheckDue,
  updatePreferences,
} from "./update-policy";

export const updatePanel = `
  <dialog id="update-dialog" aria-labelledby="update-title">
    <h2 id="update-title">Aperture updates</h2>
    <p id="update-edition"></p>
    <p id="update-status" role="status" aria-live="polite">Check for a new version.</p>
    <pre id="update-notes" hidden></pre>
    <progress id="update-download-progress" aria-label="Update download" hidden></progress>
    <label class="update-preference"><input id="update-auto" type="checkbox"> Check automatically once a day</label>
    <div class="save-actions">
      <button id="update-check" class="file-button" type="button">Check now</button>
      <button id="update-action" class="file-button save-primary" type="button" hidden>Download update</button>
      <button id="update-later" class="file-button" type="button" hidden>Later</button>
      <button id="update-releases" class="file-button" type="button">Releases</button>
      <button id="update-close" class="file-button" type="button">Close</button>
    </div>
  </dialog>
  <dialog id="update-unsaved-dialog" aria-labelledby="update-unsaved-title">
    <form method="dialog">
      <h2 id="update-unsaved-title">Unsaved annotations</h2>
      <p>Save your point annotations before restarting to install the update?</p>
      <div class="save-actions">
        <button class="file-button save-primary" type="submit" value="save">Save first</button>
        <button class="file-button" type="submit" value="discard">Discard and restart</button>
        <button class="file-button" type="submit" value="cancel" autofocus>Cancel</button>
      </div>
    </form>
  </dialog>`;

type PackageInfo = {
  edition: "installed" | "appimage" | "portable" | "development";
  version: string;
};
type UpdateInfo = { version: string; notes: string };

export function bindUpdates(
  prepare: () => Promise<boolean>,
  finish: () => void,
) {
  const get = <T extends HTMLElement = HTMLElement>(id: string) =>
    document.getElementById(id)! as T;
  const dialog = get<HTMLDialogElement>("update-dialog");
  const button = get<HTMLButtonElement>("update-button");
  const check = get<HTMLButtonElement>("update-check");
  const action = get<HTMLButtonElement>("update-action");
  const later = get<HTMLButtonElement>("update-later");
  const close = get<HTMLButtonElement>("update-close");
  const auto = get<HTMLInputElement>("update-auto");
  const status = get("update-status");
  const progress = get<HTMLProgressElement>("update-download-progress");
  let preferences = updatePreferences(null);
  try {
    preferences = updatePreferences(localStorage.getItem("aperture-updates"));
  } catch {
    /* Keep session preferences when storage is unavailable. */
  }
  const persist = () => {
    try {
      localStorage.setItem("aperture-updates", JSON.stringify(preferences));
    } catch {
      /* Session preferences still apply. */
    }
  };
  let info: PackageInfo = { edition: "development", version: "" };
  let update: UpdateInfo | null = null;
  let ready = false;
  let busy = false;
  const setBusy = (value: boolean) => {
    busy = value;
    check.disabled = value;
    action.disabled = value;
    later.disabled = value;
    close.disabled = value;
  };
  const refreshAction = () => {
    action.hidden = !update;
    later.hidden = !update;
    action.textContent =
      info.edition === "portable"
        ? "Download portable"
        : ready
          ? "Install and restart"
          : "Download update";
  };
  const show = () => {
    if (!dialog.open) dialog.showModal();
  };
  const runCheck = async (manual: boolean) => {
    if (busy) return;
    setBusy(true);
    preferences.lastAttempt = Date.now();
    persist();
    if (manual) status.textContent = "Checking for updates…";
    try {
      update = await invoke<UpdateInfo | null>("check_update");
      ready = false;
      get("update-notes").hidden = !update;
      get("update-notes").textContent = update?.notes ?? "";
      status.textContent = update
        ? `Version ${update.version} is available.`
        : "You have the latest version.";
      button.classList.toggle(
        "update-available",
        !!update && announceUpdate(update.version, preferences, manual),
      );
      button.title = update
        ? `Aperture ${update.version} is available`
        : "Aperture updates";
    } catch (error) {
      if (manual)
        status.textContent =
          info.edition === "development"
            ? "Updates are available in released packages. Use Releases to download Aperture."
            : "Could not check for updates. Try again when online, or visit Releases.";
      console.debug("Update check:", error);
    } finally {
      setBusy(false);
      refreshAction();
    }
  };
  button.addEventListener("click", show);
  close.addEventListener("click", () => dialog.close());
  dialog.addEventListener("cancel", (event) => {
    if (busy) event.preventDefault();
  });
  check.addEventListener("click", () => void runCheck(true));
  auto.checked = preferences.enabled;
  auto.addEventListener("change", () => {
    preferences.enabled = auto.checked;
    persist();
  });
  later.addEventListener("click", () => {
    preferences.deferredVersion = update?.version ?? "";
    persist();
    button.classList.remove("update-available");
    dialog.close();
  });
  get("update-releases").addEventListener("click", () => {
    void invoke("update_release_page").catch(() => {
      status.textContent = "Could not open the browser.";
    });
  });
  action.addEventListener("click", () => {
    if (busy || !update) return;
    void (async () => {
      setBusy(true);
      let prepared = false;
      try {
        if (info.edition === "portable") {
          await invoke("download_portable_update");
          status.textContent =
            "Download the new portable executable in your browser. Close Aperture, then replace your old executable. Your portable edition never runs an installer.";
        } else if (!ready) {
          status.textContent = "Downloading update…";
          progress.hidden = false;
          progress.removeAttribute("value");
          await invoke("download_update");
          ready = true;
          status.textContent =
            "Update downloaded and signature verified. Install when you are ready to restart.";
        } else {
          dialog.close();
          if (!(await prepare())) {
            show();
            return;
          }
          prepared = true;
          show();
          status.textContent = "Installing update and restarting…";
          await invoke("install_update");
        }
      } catch (error) {
        status.textContent = `Update failed: ${String(error)}`;
        if (prepared) finish();
        show();
      } finally {
        progress.hidden = true;
        setBusy(false);
        refreshAction();
      }
    })();
  });
  if (!isTauri()) {
    get("update-edition").textContent =
      "Browser preview. Updates are available in the desktop app.";
    check.disabled = true;
    get<HTMLButtonElement>("update-releases").disabled = true;
    auto.disabled = true;
    return;
  }
  void listen<{ downloaded: number; total: number | null }>(
    "update-progress",
    ({ payload }) => {
      if (payload.total) {
        progress.max = payload.total;
        progress.value = payload.downloaded;
      }
      status.textContent = payload.total
        ? `Downloading update… ${Math.min(100, Math.round((payload.downloaded / payload.total) * 100))}%`
        : `Downloading update… ${Math.round(payload.downloaded / 1024 / 1024)} MiB`;
    },
  ).catch(() => {});
  void invoke<PackageInfo>("update_package")
    .then((result) => {
      info = result;
      const label = {
        installed: "Windows installed",
        portable: "Windows portable",
        appimage: "Linux AppImage",
        development: "Development build",
      }[info.edition];
      get("update-edition").textContent = `Version ${info.version} · ${label}`;
      if (info.edition !== "development") {
        // Defer startup checks so they never delay the first image or display a startup alert.
        window.setTimeout(() => {
          if (automaticCheckDue(preferences, Date.now())) void runCheck(false);
        }, 10_000);
        window.setInterval(
          () => {
            if (automaticCheckDue(preferences, Date.now()))
              void runCheck(false);
          },
          60 * 60 * 1000,
        );
      }
    })
    .catch(() => {
      status.textContent = "Updates are unavailable in this build.";
    });
}
