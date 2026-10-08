import { icon } from "./icons";

export function bindTheme() {
  const button = document.getElementById("theme-button")!;
  const system = window.matchMedia("(prefers-color-scheme: dark)");
  let preference: string | null = null;
  try {
    preference = localStorage.getItem("aperture-theme");
  } catch {
    // System appearance still works if storage is unavailable.
  }
  const update = () => {
    const dark =
      preference === "dark" || (preference !== "light" && system.matches);
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    button.innerHTML = icon(dark ? "sun" : "moon");
    button.title = dark ? "Switch to light mode" : "Switch to dark mode";
    button.setAttribute("aria-label", button.title);
  };
  button.addEventListener("click", () => {
    preference =
      document.documentElement.dataset.theme === "dark" ? "light" : "dark";
    try {
      localStorage.setItem("aperture-theme", preference);
    } catch {
      // Keep the choice for this session even without storage.
    }
    update();
  });
  system.addEventListener("change", update);
  update();
}
