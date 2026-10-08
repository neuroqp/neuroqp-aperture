export const pointTypes = ["Cross", "Dot", "Circle"] as const;
export const pointColors = {
  Purple: "#6c3e89",
  Magenta: "#e000df",
  Red: "#e03030",
  Orange: "#f7830d",
  Yellow: "#f5d000",
  Green: "#21a34a",
  Cyan: "#00bbce",
  Blue: "#2463eb",
  White: "#ffffff",
  Black: "#000000",
};
export const pointSizes = {
  Tiny: 2,
  Small: 3.5,
  Medium: 5,
  Large: 7,
  "Extra large": 9,
};
export type PointSettings = {
  type: (typeof pointTypes)[number];
  color: keyof typeof pointColors;
  size: keyof typeof pointSizes;
};
export const defaultPointSettings: PointSettings = {
  type: "Cross",
  color: "Purple",
  size: "Medium",
};

// Each choice changes one property and previews the other two as selected.
export function pointChoices(key: keyof PointSettings, current: PointSettings) {
  const values = {
    type: pointTypes,
    color: Object.keys(pointColors),
    size: Object.keys(pointSizes),
  }[key];
  return values.map((value) => ({
    value,
    selected: current[key] === value,
    settings: { ...current, [key]: value } as PointSettings,
  }));
}

export function pointSymbol({ type, color, size }: PointSettings) {
  const radius = pointSizes[size];
  const shape =
    type === "Cross"
      ? `<path d="M${12 - radius} 12h${2 * radius}M12 ${12 - radius}v${2 * radius}"/>`
      : `<circle cx="12" cy="12" r="${radius}"${type === "Dot" ? ' fill="currentColor" stroke="none"' : ""}/>`;
  return `<svg viewBox="0 0 24 24" aria-hidden="true" data-type="${type}" data-color="${color}" data-size="${size}" style="color:${pointColors[color]}" class="point-symbol ${color === "White" ? "white-marker" : color === "Black" ? "black-marker" : ""}">${color === "White" || color === "Black" ? `<g class="marker-halo">${shape}</g>` : ""}${shape}</svg>`;
}
