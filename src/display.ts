import { channelHistograms } from "./display-settings";

export async function loadChannelHistograms(
  url: string,
  channels: number,
  signal?: AbortSignal,
) {
  const image = new Image();
  image.crossOrigin = "anonymous";
  const abort = () => {
    image.src = "";
  };
  signal?.throwIfAborted();
  signal?.addEventListener("abort", abort, { once: true });
  image.src = url;
  try {
    await image.decode();
    signal?.throwIfAborted();
  } finally {
    signal?.removeEventListener("abort", abort);
  }
  const scale = Math.min(
    1,
    256 / Math.max(image.naturalWidth, image.naturalHeight),
  );
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(image.naturalWidth * scale));
  canvas.height = Math.max(1, Math.round(image.naturalHeight * scale));
  const context = canvas.getContext("2d", { willReadFrequently: true })!;
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  const rgba = context.getImageData(0, 0, canvas.width, canvas.height).data;
  const pixels = canvas.width * canvas.height;
  return channelHistograms(
    Array.from({ length: channels }, (_, channel) =>
      Uint8Array.from({ length: pixels }, (_, i) => rgba[i * 4 + channel]),
    ),
    canvas.width,
    canvas.height,
    Uint8Array.from({ length: pixels }, (_, i) => rgba[i * 4 + 3]),
  );
}
