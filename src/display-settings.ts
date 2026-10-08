export const defaultDisplaySettings = {
  minimum: 0,
  maximum: 255,
  brightness: 0,
  contrast: 100,
};
export type DisplaySettings = typeof defaultDisplaySettings;

// UI state only. No settings are passed to the image renderer or backend.
export function adjustDisplay(
  settings: DisplaySettings,
  key: keyof DisplaySettings,
  value: number,
): DisplaySettings {
  if (!Number.isFinite(value)) return settings;
  const bounds = {
    minimum: [0, settings.maximum - 1],
    maximum: [settings.minimum + 1, 255],
    brightness: [-100, 100],
    contrast: [0, 200],
  };
  const [min, max] = bounds[key];
  return {
    ...settings,
    [key]: Math.max(min, Math.min(max, Math.round(value))),
  };
}
