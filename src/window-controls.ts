import type { Window } from "@tauri-apps/api/window";
import { icon } from "./icons";

/** Bind the integrated title-bar buttons of the image bar and the settings dialog. */
export function bindWindowControls(
  appWindow: Window | null,
  onError: (message: string) => void,
) {
  if (!appWindow) {
    document
      .querySelectorAll<HTMLButtonElement>(".window-button")
      .forEach((button) => (button.disabled = true));
    return;
  }
  const get = (id: string) => document.getElementById(id)!;
  for (const prefix of ["", "settings-"]) {
    get(`${prefix}minimize-button`).addEventListener(
      "click",
      () => void appWindow.minimize().catch(onError),
    );
    get(`${prefix}maximize-button`).addEventListener(
      "click",
      () => void appWindow.toggleMaximize().catch(onError),
    );
    get(
      prefix ? "settings-window-close-button" : "close-button",
    ).addEventListener("click", () => void appWindow.close().catch(onError));
  }
  const syncMaximize = async () => {
    const maximized = await appWindow.isMaximized();
    for (const id of ["maximize-button", "settings-maximize-button"]) {
      const button = get(id);
      button.innerHTML = icon(maximized ? "restore" : "maximize");
      button.title = maximized ? "Restore" : "Maximize";
      button.setAttribute("aria-label", button.title);
    }
  };
  void syncMaximize().catch(onError);
  void appWindow.onResized(() => void syncMaximize().catch(onError));
}
