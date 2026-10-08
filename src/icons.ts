import {
  createElement,
  FolderOpen,
  Save,
  Hand,
  ZoomIn,
  ZoomOut,
  Scan,
  X,
  Minus,
  Square,
  Copy,
  SlidersHorizontal,
  RotateCcw,
  Sun,
  Moon,
  CircleHelp,
  ChevronDown,
  type IconNode,
} from "lucide";

const icons = {
  open: FolderOpen,
  save: Save,
  hand: Hand,
  zoomIn: ZoomIn,
  zoomOut: ZoomOut,
  fit: Scan,
  cancel: X,
  minimize: Minus,
  maximize: Square,
  restore: Copy,
  brightness: SlidersHorizontal,
  reset: RotateCcw,
  sun: Sun,
  moon: Moon,
  help: CircleHelp,
  dropdown: ChevronDown,
  // Three point markers, drawn on Lucide's 24px grid.
  points: [
    ["path", { d: "M3 8h6M6 5v6M14 5h6M17 2v6M11 18h6M14 15v6" }],
  ] as IconNode,
};

export type IconName = keyof typeof icons;
export const icon = (name: IconName) =>
  createElement(icons[name], { "aria-hidden": "true", "stroke-width": 1.6 })
    .outerHTML;
