import {
  adjustDisplay,
  defaultDomain,
  displayValue,
  percentileRange,
  type DisplaySettings,
  type Domain,
} from "./display.ts";

export interface ColorChannel {
  id: string;
  name: string;
  color?: string;
  domain?: Domain;
}

export function sameColorChannels(
  previous: readonly ColorChannel[],
  next: readonly ColorChannel[],
) {
  return (
    previous.length === next.length &&
    previous.every(
      (channel, index) =>
        channel.id === next[index].id &&
        channel.name === next[index].name &&
        channel.color === next[index].color &&
        String(channel.domain ?? defaultDomain) ===
          String(next[index].domain ?? defaultDomain),
    )
  );
}

export interface ColorConnection extends DisplaySettings {
  id: number;
  source: string;
  color: string;
  brightness: number;
}

export type ColorAdjustment = DisplaySettings & { brightness: number };

const defaultColorAdjustment = ([minimum, maximum]: Domain) => ({
  minimum,
  maximum,
  brightness: 100,
});

export function colorConnectionValue(value: number, settings: ColorAdjustment) {
  return displayValue(value, settings) * (settings.brightness / 100);
}

export interface ColorSettings {
  colors: Record<string, string>;
  connections: ColorConnection[];
  /** Source sample bounds by channel id; absent channels use 0–255. */
  domains?: Record<string, Domain>;
}

const domainOf = (settings: ColorSettings, source: string) =>
  settings.domains?.[source] ?? defaultDomain;

export const colorPresets: Readonly<Record<string, string>> = {
  Red: "#ff0000",
  Green: "#00ff00",
  Blue: "#0000ff",
  Gray: "#ffffff",
  Cyan: "#00ffff",
  Magenta: "#ff00ff",
  Yellow: "#ffff00",
};

export function normalizeHex(value: string): string | null {
  let hex = value.trim().replace(/^#/, "");
  if (!/^([0-9a-f]{3}|[0-9a-f]{6})$/i.test(hex)) return null;
  if (hex.length === 3) hex = [...hex].map((digit) => digit + digit).join("");
  return "#" + hex.toLowerCase();
}

export interface HSV {
  h: number;
  s: number;
  v: number;
}

export function toHSV(hex: string): HSV {
  const [r, g, b] = hex
    .match(/[a-f0-9]{2}/gi)!
    .map((v) => parseInt(v, 16) / 255);
  const max = Math.max(r, g, b),
    min = Math.min(r, g, b),
    delta = max - min;
  const h = !delta
    ? 0
    : max === r
      ? ((g - b) / delta) % 6
      : max === g
        ? (b - r) / delta + 2
        : (r - g) / delta + 4;
  return { h: (h * 60 + 360) % 360, s: max ? delta / max : 0, v: max };
}

export function toHex({ h, s, v }: HSV): string {
  const component = (n: number) => {
    const k = (n + h / 60) % 6;
    return Math.round(255 * (v - v * s * Math.max(0, Math.min(k, 4 - k, 1))))
      .toString(16)
      .padStart(2, "0");
  };
  return "#" + component(5) + component(3) + component(1);
}

export function colorName(hex: string): string {
  if (hex.toLowerCase() === "#808080") return "Gray";
  const pure = Object.entries(colorPresets).find(
    ([, value]) => value === hex.toLowerCase(),
  );
  if (pure) return pure[0];
  const { h, s } = toHSV(hex);
  const family =
    s < 0.12
      ? "Gray"
      : ["Red", "Yellow", "Green", "Cyan", "Blue", "Magenta"][
          Math.floor((h + 30) / 60) % 6
        ];
  return family + "-ish";
}

export function defaultColorSettings(
  channels: readonly ColorChannel[],
): ColorSettings {
  const colors = { ...colorPresets };
  const connections = channels.map((channel, index) => {
    const hex = channel.color && normalizeHex(channel.color);
    let color =
      Object.entries(colors).find(
        ([name, value]) =>
          name.toLowerCase() === channel.color?.toLowerCase() || value === hex,
      )?.[0] ?? "Gray";
    if (hex && !Object.values(colors).includes(hex)) {
      color = `channel:${channel.id}`;
      colors[color] = hex;
    }
    return {
      id: index + 1,
      source: channel.id,
      color,
      ...defaultColorAdjustment(channel.domain ?? defaultDomain),
    };
  });
  const domains = Object.fromEntries(
    channels.map((channel) => [channel.id, channel.domain ?? defaultDomain]),
  );
  return { colors, connections, domains };
}

export const copyColorSettings = (settings: ColorSettings): ColorSettings => ({
  ...settings,
  colors: { ...settings.colors },
  connections: settings.connections.map((connection) => ({ ...connection })),
});

export function connectColor(
  settings: ColorSettings,
  source: string,
  color: string,
  id?: number,
): ColorSettings {
  if (
    !(color in settings.colors) ||
    settings.connections.some(
      (edge) =>
        edge.source === source && edge.color === color && edge.id !== id,
    )
  )
    return settings;
  const edge = settings.connections.find((edge) => edge.id === id);
  if (
    id !== undefined &&
    (!edge || edge.source !== source || edge.color === color)
  )
    return settings;
  return {
    ...settings,
    connections: edge
      ? settings.connections.map((item) =>
          item.id === id ? { ...item, color } : item,
        )
      : [
          ...settings.connections,
          {
            id: Math.max(0, ...settings.connections.map((item) => item.id)) + 1,
            source,
            color,
            ...defaultColorAdjustment(domainOf(settings, source)),
          },
        ],
  };
}

export function removeColorConnection(
  settings: ColorSettings,
  id: number,
): ColorSettings {
  return {
    ...settings,
    connections: settings.connections.filter((edge) => edge.id !== id),
  };
}

export function adjustColorConnection(
  settings: ColorSettings,
  id: number,
  field: keyof ColorAdjustment,
  value: number,
): ColorSettings {
  if (!Number.isFinite(value)) return settings;
  return {
    ...settings,
    connections: settings.connections.map((edge) =>
      edge.id === id
        ? {
            ...edge,
            ...(field === "brightness"
              ? { brightness: Math.max(0, Math.min(100, Math.round(value))) }
              : adjustDisplay(
                  edge,
                  field,
                  value,
                  domainOf(settings, edge.source),
                )),
          }
        : edge,
    ),
  };
}

export function setColorConnectionRange(
  settings: ColorSettings,
  id: number,
  range: DisplaySettings,
): ColorSettings {
  if (!Number.isFinite(range.minimum) || !Number.isFinite(range.maximum))
    return settings;
  const bounded = (domain: Domain) =>
    adjustDisplay(
      adjustDisplay(
        { minimum: domain[0], maximum: domain[1] },
        "minimum",
        range.minimum,
        domain,
      ),
      "maximum",
      range.maximum,
      domain,
    );
  return {
    ...settings,
    connections: settings.connections.map((edge) =>
      edge.id === id
        ? { ...edge, ...bounded(domainOf(settings, edge.source)) }
        : edge,
    ),
  };
}

/** Start each connection at its channel's sampled occupied range (0–100 %). */
export function initialColorRanges(
  settings: ColorSettings,
  channels: readonly ColorChannel[],
  histograms: readonly (ArrayLike<number> | null)[],
  rgb: boolean,
): ColorSettings {
  const ranges = new Map<string, DisplaySettings>();
  if (rgb) {
    // One shared range across R, G and B preserves the color balance.
    const sum = new Float64Array(histograms.find(Boolean)?.length ?? 0);
    for (const bins of histograms.slice(0, 3))
      for (let value = 0; value < sum.length; value++)
        sum[value] += bins?.[value] ?? 0;
    const range = percentileRange(sum, 0, channels[0]?.domain);
    for (const channel of channels.slice(0, 3)) ranges.set(channel.id, range);
  } else
    channels.forEach((channel, index) => {
      const bins = histograms[index];
      if (bins)
        ranges.set(channel.id, percentileRange(bins, 0, channel.domain));
    });
  return {
    ...settings,
    connections: settings.connections.map((edge) => ({
      ...edge,
      ...ranges.get(edge.source),
    })),
  };
}

export function resetColorConnection(
  settings: ColorSettings,
  id: number,
): ColorSettings {
  return {
    ...settings,
    connections: settings.connections.map((edge) =>
      edge.id === id
        ? {
            ...edge,
            ...defaultColorAdjustment(domainOf(settings, edge.source)),
          }
        : edge,
    ),
  };
}

export function setOutputColor(
  settings: ColorSettings,
  color: string,
  value: string,
): ColorSettings {
  const hex = normalizeHex(value);
  return hex && color in settings.colors
    ? { ...settings, colors: { ...settings.colors, [color]: hex } }
    : settings;
}

export function visibleChannelSettings(
  settings: ColorSettings,
  channel: number,
): ColorSettings {
  return channel < 0
    ? settings
    : {
        ...settings,
        connections: settings.connections.filter(
          (connection) => connection.source === String(channel),
        ),
      };
}
