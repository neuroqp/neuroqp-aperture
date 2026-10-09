export interface ColorChannel {
  id: string;
  name: string;
  color?: string;
}

export interface ColorConnection {
  id: number;
  source: string;
  color: string;
  brightness: number;
  contrast: number;
}

export interface ColorSettings {
  colors: Record<string, string>;
  connections: ColorConnection[];
}

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
      brightness: 0,
      contrast: 100,
    };
  });
  return { colors, connections };
}

export const copyColorSettings = (settings: ColorSettings): ColorSettings => ({
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
            brightness: 0,
            contrast: 100,
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
  field: "brightness" | "contrast",
  value: number,
): ColorSettings {
  if (!Number.isFinite(value)) return settings;
  const bounded = Math.round(
    Math.max(
      field === "brightness" ? -100 : 0,
      Math.min(field === "brightness" ? 100 : 200, value),
    ),
  );
  return {
    ...settings,
    connections: settings.connections.map((edge) =>
      edge.id === id ? { ...edge, [field]: bounded } : edge,
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
