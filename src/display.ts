export const defaultDisplaySettings = {
  minimum: 0,
  maximum: 255,
};
export type DisplaySettings = typeof defaultDisplaySettings;
/** Inclusive source sample bounds of a channel. */
export type Domain = readonly [number, number];
export const defaultDomain: Domain = [0, 255];

// Display settings never alter source pixels or cached preview/tile files.
export function adjustDisplay(
  settings: DisplaySettings,
  key: keyof DisplaySettings,
  value: number,
  domain: Domain = defaultDomain,
): DisplaySettings {
  if (!Number.isFinite(value)) return settings;
  const bounds = {
    minimum: [domain[0], settings.maximum - 1],
    maximum: [settings.minimum + 1, domain[1]],
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
// 16-bit previews count every pixel so sparse bright spots set the sampled maximum.
export function channelHistograms(
  planes: readonly ArrayLike<number>[],
  width: number,
  height: number,
  alpha?: ArrayLike<number>,
  bits: 8 | 16 = 8,
) {
  const bins = planes.map(() => new Uint32Array(1 << bits));
  const step =
    bits === 16 ? 1 : Math.max(1, Math.ceil(Math.max(width, height) / 256));
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

export function percentileRange(
  bins: ArrayLike<number>,
  tail: number,
  domain: Domain = defaultDomain,
) {
  let total = 0;
  for (let i = 0; i < bins.length; i++) total += bins[i];
  if (total === 0) return { minimum: domain[0], maximum: domain[1] };
  const quantile = (fraction: number) => {
    const rank = Math.max(1, Math.ceil(total * fraction));
    let count = 0;
    for (let i = 0; i < bins.length; i++) {
      count += bins[i];
      if (count >= rank) return Math.max(domain[0], Math.min(domain[1], i));
    }
    return domain[1];
  };
  const low = quantile(tail / 100);
  const high = quantile(1 - tail / 100);
  if (high > low) return { minimum: low, maximum: high };
  // Constant images still need a finite one-unit transfer function.
  return low < domain[1]
    ? { minimum: low, maximum: low + 1 }
    : { minimum: low - 1, maximum: low };
}

/**
 * Histogram x-axis in source units. Wide domains end at the smallest
 * all-ones value (e.g. 4095, 16383) covering the occupied samples and the
 * current maximum, so dim 16-bit data stays readable instead of hugging zero.
 */
export function histogramAxis(
  bins: ArrayLike<number> | null,
  domain: Domain,
  maximum: number,
): Domain {
  let top = maximum;
  for (let i = (bins?.length ?? 0) - 1; i > top; i--)
    if (bins![i]) {
      top = i;
      break;
    }
  const end = 2 ** Math.ceil(Math.log2(top + 1)) - 1;
  return [
    domain[0],
    Math.max(Math.min(255, domain[1]), Math.min(end, domain[1])),
  ];
}

/** Sum source bins into `count` equal display bins across an inclusive axis. */
export function displayBins(
  bins: ArrayLike<number>,
  [start, end]: Domain,
  count = 256,
) {
  const result = new Float64Array(count);
  const width = (end - start + 1) / count;
  for (let value = start; value <= end && value < bins.length; value++)
    result[Math.min(count - 1, Math.floor((value - start) / width))] +=
      bins[value];
  return result;
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
