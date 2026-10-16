export function bindTheme() {
  const select = document.getElementById("theme-select") as HTMLSelectElement;
  const system = window.matchMedia("(prefers-color-scheme: dark)");
  let preference = "system";
  try {
    const stored = localStorage.getItem("aperture-theme");
    if (stored === "light" || stored === "dark") preference = stored;
  } catch {
    // System appearance still works if storage is unavailable.
  }
  const update = () => {
    const dark =
      preference === "dark" || (preference === "system" && system.matches);
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    select.value = preference;
  };
  select.addEventListener("change", () => {
    preference = select.value;
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
