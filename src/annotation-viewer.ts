import OpenSeadragon from "openseadragon";
import { pointSizes } from "./point-settings";
import {
  imageToStored,
  onFirstPage,
  numberOfCounters,
  pointAppearance,
  storedToImage,
  type ImageBounds,
  type PointRoi,
  type PointReference,
} from "./annotations";

import {
  hitPoint,
  pointAction,
  keyPlacementAllowed,
  type ScreenPoint,
} from "./point-interaction";

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
  private points: ScreenPoint[] = [];
  private pointer: OpenSeadragon.Point | null = null;
  private space = false;
  private ctrl = false;
  private alt = false;
  private pressed = false;
  private buttons = 0;
  private dragged:
    (PointReference & { offsetX: number; offsetY: number }) | null = null;
  private editedGesture = false;

  constructor(
    viewer: OpenSeadragon.Viewer,
    private actions: {
      add: (point: { x: number; y: number }) => void;
      move: (
        reference: PointReference,
        point: { x: number; y: number },
      ) => void;
      delete: (reference: PointReference) => void;
      active: () => boolean;
      editable: () => boolean;
    },
  ) {
    this.viewer = viewer;
    this.overlay.setAttribute(
      "style",
      "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;overflow:hidden",
    );
    viewer.container.append(this.overlay);
    viewer.addHandler("canvas-press", this.press);
    viewer.addHandler("canvas-drag", this.drag);
    viewer.addHandler("canvas-drag-end", this.dragEnd);
    viewer.addHandler("canvas-release", this.release);
    viewer.addHandler("canvas-click", this.click);
    viewer.addHandler("canvas-exit", this.exit);
    viewer.addHandler("canvas-nonprimary-press", this.buttonsChange);
    viewer.addHandler("canvas-nonprimary-release", this.buttonsChange);
    viewer.canvas.addEventListener("pointermove", this.pointerMove);
    viewer.canvas.addEventListener("pointercancel", this.resetInteraction);
    document.addEventListener("keydown", this.keyDown, true);
    document.addEventListener("keyup", this.keyUp, true);
    window.addEventListener("blur", this.resetInteraction);
    viewer.addHandler("animation", this.render);
    viewer.addHandler("resize", this.render);
    viewer.addHandler("open", this.render);
    viewer.world.addHandler("add-item", this.render);
    this.updateInteraction();
  }

  private toStored(position: OpenSeadragon.Point) {
    const item = this.viewer.world.getItemAt(0);
    if (!this.image || !item) return null;
    const viewportPoint = this.viewer.viewport.pointFromPixel(position, true);
    const point = item.viewportToImageCoordinates(viewportPoint, true);
    return imageToStored(point, this.image.width, item.getFlip());
  }

  private modifiers(event: MouseEvent) {
    this.ctrl = event.ctrlKey;
    this.buttons = event.buttons;
    this.alt = event.altKey;
  }

  private press = (event: OpenSeadragon.CanvasPressEvent) => {
    this.modifiers(event.originalEvent as MouseEvent);
    this.pointer = event.position;
    this.pressed = true;
    this.editedGesture = false;
    const hit = hitPoint(this.points, event.position);
    const active = this.actions.active() && this.actions.editable();
    const action = pointAction(
      active,
      this.space || this.ctrl,
      this.alt,
      !!hit,
    );
    this.editedGesture = action !== "pan";
    if (action === "delete" && hit) this.actions.delete(hit);
    else if (action === "move" && hit) {
      this.dragged = {
        ...hit,
        offsetX: hit.x - event.position.x,
        offsetY: hit.y - event.position.y,
      };
    } else if (action === "place") {
      const point = this.toStored(event.position);
      if (point) this.actions.add(point);
    }
    this.updateInteraction();
  };

  private drag = (event: OpenSeadragon.CanvasDragEvent) => {
    this.modifiers(event.originalEvent as MouseEvent);
    this.pointer = event.position;
    this.updateInteraction();
    const pan =
      this.space ||
      this.ctrl ||
      !this.actions.active() ||
      !this.actions.editable();
    event.preventDefaultAction = !pan;
    if (!pan && this.dragged && this.actions.editable()) {
      const point = this.toStored(
        new OpenSeadragon.Point(
          event.position.x + this.dragged.offsetX,
          event.position.y + this.dragged.offsetY,
        ),
      );
      if (point) this.actions.move(this.dragged, point);
    }
  };

  private dragEnd = (event: Omit<OpenSeadragon.CanvasDragEvent, "delta">) => {
    // An annotation gesture must never finish with OSD's momentum pan.
    event.preventDefaultAction = this.editedGesture;
  };

  private release = (event: OpenSeadragon.CanvasReleaseEvent) => {
    this.modifiers(event.originalEvent as MouseEvent);
    this.pressed = false;
    this.dragged = null;
    this.pointer = event.insideElementReleased ? event.position : null;
    this.updateInteraction();
  };

  private click = (event: OpenSeadragon.CanvasClickEvent) => {
    event.preventDefaultAction = this.actions.active();
  };

  private pointerMove = (event: PointerEvent) => {
    this.modifiers(event);
    const bounds = this.viewer.canvas.getBoundingClientRect();
    this.pointer = new OpenSeadragon.Point(
      event.clientX - bounds.left,
      event.clientY - bounds.top,
    );
    this.updateInteraction();
  };

  private exit = () => {
    if (!this.pressed) this.pointer = null;
    this.updateInteraction();
  };

  private buttonsChange = (
    event: OpenSeadragon.CanvasNonPrimaryButtonEvent,
  ) => {
    this.buttons = event.buttons;
  };

  private keyDown = (event: KeyboardEvent) => {
    if (document.querySelector("dialog[open]")) return;
    if (event.key.toLowerCase() === "e") {
      const typing =
        event.target instanceof Element &&
        !!event.target.closest(
          "input, select, textarea, [contenteditable], [role=listbox], [role=combobox]",
        );
      if (
        keyPlacementAllowed({
          active: this.actions.active(),
          editable: this.actions.editable(),
          hovering: this.pointer !== null,
          repeat: event.repeat,
          modified:
            this.space ||
            this.ctrl ||
            this.alt ||
            event.ctrlKey ||
            event.altKey ||
            event.metaKey,
          pressed: this.pressed || this.buttons !== 0,
          typing,
        }) &&
        this.pointer
      ) {
        const point = this.toStored(this.pointer);
        if (
          point &&
          this.image &&
          point.x >= -0.5 &&
          point.y >= -0.5 &&
          point.x < this.image.width - 0.5 &&
          point.y < this.image.height - 0.5
        ) {
          event.preventDefault();
          this.actions.add(point);
        }
      }
      return;
    }
    if (
      event.code === "Space" &&
      event.target instanceof Element &&
      event.target.closest("[role=listbox]")
    )
      return;
    const control =
      event.target instanceof Element
        ? event.target.closest(
            "input, select, textarea, button, [contenteditable], [role=listbox]",
          )
        : null;
    if (
      event.code === "Space" &&
      control &&
      (!this.pointer ||
        control.matches("input, select, textarea, [contenteditable]"))
    )
      return;
    if (event.code === "Space") {
      this.space = true;
      event.preventDefault();
    }
    if (event.key === "Control") this.ctrl = true;
    if (event.key === "Alt") this.alt = true;
    this.updateInteraction();
  };

  private keyUp = (event: KeyboardEvent) => {
    if (event.code === "Space") this.space = false;
    if (event.key === "Control") this.ctrl = event.ctrlKey;
    if (event.key === "Alt") this.alt = event.altKey;
    this.updateInteraction();
  };

  private resetInteraction = () => {
    this.space = this.ctrl = this.alt = this.pressed = false;
    this.buttons = 0;
    this.dragged = null;
    this.pointer = null;
    this.updateInteraction();
  };

  updateInteraction = () => {
    const active = this.actions.active() && this.actions.editable();
    const pan = this.space || this.ctrl || !active;
    if (pan || this.alt) this.dragged = null;
    for (const type of ["mouse", "touch", "pen"] as const) {
      const gestures = this.viewer.gestureSettingsByDeviceType(type);
      gestures.dragToPan = pan;
      gestures.dblClickToZoom = !active;
    }
    const hit = this.pointer && hitPoint(this.points, this.pointer);
    const action = pointAction(active, pan, this.alt, !!hit);
    this.viewer.element.dataset.interaction =
      this.pressed && pan
        ? "panning"
        : this.dragged
          ? "moving"
          : action === "place" || action === "none"
            ? "point"
            : action;
  };

  setImage(image: ImageBounds | null) {
    if (
      !image ||
      !this.image ||
      image.width !== this.image.width ||
      image.height !== this.image.height
    )
      this.resetInteraction();
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
    this.points = [];
    if (!item || !this.image) return;
    const fragment = document.createDocumentFragment();
    for (const [roiIndex, roi] of this.rois.entries()) {
      if (!onFirstPage(roi)) continue;
      const radius =
        [...Object.values(pointSizes), 12, 16][roi.size] ?? pointSizes.Medium;
      const multiple = numberOfCounters(roi) > 1;
      for (const [index, point] of roi.points.entries()) {
        const { color, label } = pointAppearance(roi, index, multiple);
        if (point.position > 1) continue;
        const stored = storedToImage(point, this.image.width, item.getFlip());
        const viewportPoint = item.imageToViewportCoordinates(
          stored.x,
          stored.y,
          true,
        );
        const screen = this.viewer.viewport.pixelFromPoint(viewportPoint, true);
        this.points.push({
          roiIndex,
          pointIndex: index,
          x: screen.x,
          y: screen.y,
        });
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
        if (label !== null) {
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
          text.textContent = label;
          group.append(text);
        }
        fragment.append(group);
      }
    }
    this.overlay.append(fragment);
    this.updateInteraction();
  };

  dispose() {
    this.viewer.removeHandler("canvas-press", this.press);
    this.viewer.removeHandler("canvas-drag", this.drag);
    this.viewer.removeHandler("canvas-drag-end", this.dragEnd);
    this.viewer.removeHandler("canvas-release", this.release);
    this.viewer.removeHandler("canvas-click", this.click);
    this.viewer.removeHandler("canvas-exit", this.exit);
    this.viewer.removeHandler("canvas-nonprimary-press", this.buttonsChange);
    this.viewer.removeHandler("canvas-nonprimary-release", this.buttonsChange);
    this.viewer.canvas.removeEventListener("pointermove", this.pointerMove);
    this.viewer.canvas.removeEventListener(
      "pointercancel",
      this.resetInteraction,
    );
    document.removeEventListener("keydown", this.keyDown, true);
    document.removeEventListener("keyup", this.keyUp, true);
    window.removeEventListener("blur", this.resetInteraction);
    for (const event of ["animation", "resize", "open"] as const)
      this.viewer.removeHandler(event, this.render);
    this.viewer.world.removeHandler("add-item", this.render);
    this.overlay.remove();
  }
}
