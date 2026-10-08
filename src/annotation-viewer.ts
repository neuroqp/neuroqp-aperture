import OpenSeadragon from "openseadragon";
import { pointSizes } from "./point-settings";
import {
  imageToStored,
  storedToImage,
  type ImageBounds,
  type PointRoi,
} from "./annotations";

const ns = "http://www.w3.org/2000/svg";
const element = (name: string, attributes: Record<string, string | number>) => {
  const node = document.createElementNS(ns, name);
  for (const [key, value] of Object.entries(attributes))
    node.setAttribute(key, String(value));
  return node;
};

export class AnnotationViewer {
  private image: ImageBounds | null = null;
  private rois: PointRoi[] = [];
  private overlay = element("svg", {
    "aria-label": "Point annotations",
    "data-annotations": "",
  });
  private viewer: OpenSeadragon.Viewer;
  private click: (event: OpenSeadragon.CanvasClickEvent) => void;

  constructor(
    viewer: OpenSeadragon.Viewer,
    onPoint: (point: { x: number; y: number }) => void,
    isActive: () => boolean,
  ) {
    this.viewer = viewer;
    this.overlay.setAttribute(
      "style",
      "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;overflow:hidden",
    );
    viewer.container.append(this.overlay);
    this.click = (event) => {
      const item = viewer.world.getItemAt(0);
      if (!event.quick || !isActive() || !this.image || !item) return;
      const original = event.originalEvent as MouseEvent;
      if (typeof original.button === "number" && original.button !== 0) return;
      event.preventDefaultAction = true;
      const viewportPoint = viewer.viewport.pointFromPixel(
        event.position,
        true,
      );
      const point = item.viewportToImageCoordinates(viewportPoint, true);
      onPoint(imageToStored(point, this.image.width, item.getFlip()));
    };
    viewer.addHandler("canvas-click", this.click);
    viewer.addHandler("animation", this.render);
    viewer.addHandler("resize", this.render);
    viewer.addHandler("open", this.render);
    viewer.world.addHandler("add-item", this.render);
  }

  setImage(image: ImageBounds | null) {
    this.image = image;
    this.render();
  }
  setRois(rois: PointRoi[]) {
    this.rois = rois;
    this.render();
  }

  render = () => {
    const item = this.viewer.world.getItemAt(0);
    this.overlay.replaceChildren();
    if (!item || !this.image) return;
    let label = 0;
    const fragment = document.createDocumentFragment();
    for (const roi of this.rois) {
      if (roi.position > 1 || roi.slice > 1 || roi.frame > 1 || roi.channel > 1)
        continue;
      const radius =
        [...Object.values(pointSizes), 12, 16][roi.size] ?? pointSizes.Medium;
      const color =
        roi.color === null
          ? "#ffff00"
          : `#${(roi.color & 0xffffff).toString(16).padStart(6, "0")}`;
      for (const point of roi.points) {
        label++;
        if (point.position > 1) continue;
        const stored = storedToImage(point, this.image.width, item.getFlip());
        const viewportPoint = item.imageToViewportCoordinates(
          stored.x,
          stored.y,
          true,
        );
        const screen = this.viewer.viewport.pixelFromPoint(viewportPoint, true);
        const group = element("g", {
          transform: `translate(${screen.x} ${screen.y})`,
          stroke: color,
          "stroke-width": 1.5,
          fill: "none",
          "data-counter": point.counter,
        });
        if (roi.pointType === 0 || roi.pointType === 1)
          group.append(
            element("path", {
              d: `M${-radius} 0H${radius}M0 ${-radius}V${radius}`,
            }),
          );
        if (roi.pointType === 0 || roi.pointType === 2 || roi.pointType === 3)
          group.append(
            element("circle", {
              cx: 0,
              cy: 0,
              r: roi.pointType === 0 ? 1 : radius,
              fill: roi.pointType === 3 ? "none" : color,
              stroke: roi.pointType === 3 ? color : "none",
            }),
          );
        if (roi.showLabels) {
          const text = element("text", {
            x: radius + 4,
            y: -radius - 2,
            fill: color,
            stroke: "#000",
            "stroke-width": 2.5,
            "paint-order": "stroke",
            "font-size": 12,
            "font-family": "sans-serif",
          });
          text.textContent = String(label);
          group.append(text);
        }
        fragment.append(group);
      }
    }
    this.overlay.append(fragment);
  };

  dispose() {
    this.viewer.removeHandler("canvas-click", this.click);
    for (const event of ["animation", "resize", "open"] as const)
      this.viewer.removeHandler(event, this.render);
    this.viewer.world.removeHandler("add-item", this.render);
    this.overlay.remove();
  }
}
