import {
  displayFilter,
  histogram,
  type DisplaySettings,
} from "./display-settings";

export function applyDisplay(settings: DisplaySettings) {
  const viewer = document.getElementById("viewer")!;
  viewer.style.setProperty("--display-filter", displayFilter(settings));
  // Filter only OSD's image canvas. Identity avoids extra compositing work.
  const identity = settings.minimum === 0 && settings.maximum === 255;
  viewer.classList.toggle("display-adjusted", !identity);
}

export async function loadHistogram(url: string) {
  const image = new Image();
  image.crossOrigin = "anonymous";
  image.src = url;
  await image.decode();
  const scale = Math.min(
    1,
    256 / Math.max(image.naturalWidth, image.naturalHeight),
  );
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(image.naturalWidth * scale));
  canvas.height = Math.max(1, Math.round(image.naturalHeight * scale));
  const context = canvas.getContext("2d", { willReadFrequently: true })!;
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  return histogram(
    context.getImageData(0, 0, canvas.width, canvas.height).data,
  );
}
