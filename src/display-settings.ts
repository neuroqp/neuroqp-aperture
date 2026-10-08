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

// CSS brightness followed by contrast implements the selected black/white points.
// The first stage can only clip values already above the chosen white point.
export function displayFilter(settings: DisplaySettings) {
  const sum = settings.minimum + settings.maximum;
  const width = settings.maximum - settings.minimum;
  return `brightness(${255 / sum}) contrast(${sum / width})`;
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

// Luminance in the existing 8-bit display preview, never source-file intensities.
export function histogram(rgba: ArrayLike<number>) {
  const bins = Array<number>(256).fill(0);
  for (let i = 0; i + 3 < rgba.length; i += 4) {
    if (rgba[i + 3] === 0) continue;
    const value = Math.round(
      0.2126 * rgba[i] + 0.7152 * rgba[i + 1] + 0.0722 * rgba[i + 2],
    );
    bins[value]++;
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
