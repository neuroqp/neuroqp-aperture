export const defaultDisplaySettings = {
  minimum: 0,
  maximum: 255,
};
export type DisplaySettings = typeof defaultDisplaySettings;

// Display settings never alter source pixels or cached preview/tile files.
export function adjustDisplay(
  settings: DisplaySettings,
  key: keyof DisplaySettings,
  value: number,
): DisplaySettings {
  if (!Number.isFinite(value)) return settings;
  const bounds = {
    minimum: [0, settings.maximum - 1],
    maximum: [settings.minimum + 1, 255],
  };
  const [min, max] = bounds[key];
  return {
    ...settings,
    [key]: Math.max(min, Math.min(max, Math.round(value))),
  };
}

export function displayValue(value: number, settings: DisplaySettings) {
  return Math.max(
    0,
    Math.min(
      1,
      (value - settings.minimum) / (settings.maximum - settings.minimum),
    ),
  );
}

// Sample original display planes once, independently of their color/range mapping.
export function channelHistograms(
  planes: readonly ArrayLike<number>[],
  width: number,
  height: number,
  alpha?: ArrayLike<number>,
) {
  const bins = planes.map(() => new Uint32Array(256));
  const step = Math.max(1, Math.ceil(Math.max(width, height) / 256));
  for (let y = 0; y < height; y += step) {
    for (let x = 0; x < width; x += step) {
      const pixel = y * width + x;
      if (alpha?.[pixel] === 0) continue;
      for (let channel = 0; channel < planes.length; channel++)
        bins[channel][planes[channel][pixel]]++;
    }
  }
  return bins;
}

export function percentileRange(bins: number[], tail: number) {
  const total = bins.reduce((sum, count) => sum + count, 0);
  if (total === 0) return { minimum: 0, maximum: 255 };
  const quantile = (fraction: number) => {
    const rank = Math.max(1, Math.ceil(total * fraction));
    let count = 0;
    for (let i = 0; i < bins.length; i++) {
      count += bins[i];
      if (count >= rank) return i;
    }
    return 255;
  };
  const low = quantile(tail / 100);
  const high = quantile(1 - tail / 100);
  // Constant images still need a finite transfer function.
  return {
    minimum: Math.min(low, 254),
    maximum: Math.max(high, Math.min(low, 254) + 1),
  };
}

/** Copy one interleaved RGBA component into its own plane. */
export function component(rgba: ArrayLike<number>, index: number) {
  return Uint8Array.from(
    { length: rgba.length / 4 },
    (_, i) => rgba[i * 4 + index],
  );
}

/** Copy the first `count` interleaved RGBA components into separate planes. */
export function splitChannels(rgba: ArrayLike<number>, count: number) {
  return Array.from({ length: count }, (_, index) => component(rgba, index));
}

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
  return channelHistograms(
    splitChannels(rgba, channels),
    canvas.width,
    canvas.height,
    component(rgba, 3),
  );
}
