import type OpenSeadragon from "openseadragon";
import type { ColorSettings } from "./color-settings";
import { colorConnectionValue, copyColorSettings } from "./color-settings.ts";
import { component, splitChannels } from "./display.ts";
import type { ImageState } from "./image";

export const planeType = "aperturePlanes";
export type Planes = {
  width: number;
  height: number;
  bits: 8 | 16;
  planes: (Uint8Array | Uint16Array)[];
};
type ColorSource = OpenSeadragon.TileSourceOptions & {
  getTileUrl: (level: number, x: number, y: number) => string;
  apertureChannelCount?: number;
  apertureChannelMode?: boolean;
};

export type ColorRoute = {
  channel: number;
  minimum: number;
  maximum: number;
  brightness: number;
  /** Output color components, 0–255. */
  color: number[];
};

/** Valid connections from existing channels, in the form both renderers use. */
export function colorRoutes(
  settings: ColorSettings,
  count: number,
): ColorRoute[] {
  return settings.connections.flatMap((route) => {
    const channel = Number(route.source);
    const hex = settings.colors[route.color];
    if (
      !Number.isInteger(channel) ||
      channel < 0 ||
      channel >= count ||
      !/^#[0-9a-f]{6}$/i.test(hex ?? "")
    )
      return [];
    const color = [1, 3, 5].map((start) =>
      parseInt(hex.slice(start, start + 2), 16),
    );
    const { minimum, maximum, brightness } = route;
    return [{ channel, minimum, maximum, brightness, color }];
  });
}

/** Routes add after their own transfer function, then clamp once at output. */
export function composeColorPixels(
  planes: readonly ArrayLike<number>[],
  settings: ColorSettings,
  alpha?: Uint8Array,
) {
  const size = planes[0]?.length ?? 0;
  if (planes.some((plane) => plane.length !== size))
    throw new Error("Channel tile dimensions differ.");
  const routes = colorRoutes(settings, planes.length);
  const output = new Uint8ClampedArray(size * 4);
  for (let pixel = 0; pixel < size; pixel++) {
    let red = 0,
      green = 0,
      blue = 0;
    for (const route of routes) {
      // Same analytic transfer as the GPU shader, in source units.
      const intensity = colorConnectionValue(
        planes[route.channel][pixel],
        route,
      );
      red += intensity * route.color[0];
      green += intensity * route.color[1];
      blue += intensity * route.color[2];
    }
    const at = pixel * 4;
    output[at] = red;
    output[at + 1] = green;
    output[at + 2] = blue;
    output[at + 3] = alpha?.[pixel] ?? 255;
  }
  return output;
}

function rendered(planes: Planes, settings: ColorSettings, alpha?: Uint8Array) {
  const canvas = document.createElement("canvas");
  canvas.width = planes.width;
  canvas.height = planes.height;
  const context = canvas.getContext("2d")!;
  context.putImageData(
    new ImageData(
      composeColorPixels(planes.planes, settings, alpha),
      planes.width,
      planes.height,
    ),
    0,
    0,
  );
  return context;
}

function identity(settings: ColorSettings, count: number) {
  const colors = count === 1 ? ["#ffffff"] : ["#ff0000", "#00ff00", "#0000ff"];
  return (
    settings.connections.length === count &&
    colors.length === count &&
    colors.every((color, channel) => {
      const routes = settings.connections.filter(
        (route) => route.source === String(channel),
      );
      return (
        routes.length === 1 &&
        settings.colors[routes[0].color]?.toLowerCase() === color &&
        routes[0].minimum === 0 &&
        routes[0].maximum === 255 &&
        routes[0].brightness === 100
      );
    })
  );
}

/** Parse a `.u16` body: u32 LE width, u32 LE height, then u16 LE samples. */
export function parseU16Plane(buffer: ArrayBuffer) {
  const error = new Error("Channel tile has an unexpected size.");
  if (buffer.byteLength < 8) throw error;
  const header = new DataView(buffer, 0, 8);
  const width = header.getUint32(0, true);
  const height = header.getUint32(4, true);
  if (!width || !height || buffer.byteLength !== 8 + 2 * width * height)
    throw error;
  // Supported desktop targets are little-endian, so samples need no swapping.
  return { width, height, plane: new Uint16Array(buffer, 8, width * height) };
}

async function channelPlane(url: string, signal: AbortSignal, bits: 8 | 16) {
  const response = await fetch(url, { signal });
  if (!response.ok)
    throw new Error(`Channel tile failed (${response.status}).`);
  if (bits === 16) return parseU16Plane(await response.arrayBuffer());
  const bitmap = await createImageBitmap(await response.blob(), {
    colorSpaceConversion: "none",
  });
  const canvas = document.createElement("canvas");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  const context = canvas.getContext("2d", { willReadFrequently: true })!;
  try {
    context.drawImage(bitmap, 0, 0);
    const rgba = context.getImageData(0, 0, canvas.width, canvas.height).data;
    return {
      width: bitmap.width,
      height: bitmap.height,
      plane: component(rgba, 0),
    };
  } finally {
    bitmap.close();
    canvas.width = canvas.height = 0;
  }
}

export function bindColorRenderer(
  viewer: OpenSeadragon.Viewer,
  getSettings: () => ColorSettings,
  onError: (message: string) => void,
  onPreviewPlanes?: (imageId: string, planes: Planes) => void,
) {
  // Lazy import keeps the pixel compositor usable in Node tests without a DOM.
  const converterReady = import("openseadragon").then(({ default: osd }) => {
    // Plane arrays remain immutable; copying a reference is sufficient.
    osd.converter.learn(planeType, planeType, (_, data) => data);
    osd.converter.learn(
      planeType,
      "context2d",
      (_, data: Planes) => rendered(data, getSettings()),
      1,
      1,
    );
  });
  let frame = 0;
  let revision = 0;
  const handleTile = async (event: OpenSeadragon.TileInvalidatedEvent) => {
    if (viewer.drawer.getType() === "aperture-gpu") return;
    const source = event.tiledImage.source as OpenSeadragon.TileSource &
      ColorSource;
    const count = source.apertureChannelCount;
    if (!count || viewer.world.getIndexOfItem(event.tiledImage) < 0) return;
    const settings = copyColorSettings(getSettings());
    if (!source.apertureChannelMode && identity(settings, count)) return;
    const started = revision;
    let data: Planes;
    let alpha: Uint8Array | undefined;
    try {
      if (source.apertureChannelMode) {
        data = await event.getData(planeType);
      } else {
        const context: CanvasRenderingContext2D =
          await event.getData("context2d");
        const { width, height } = context.canvas;
        const rgba = context.getImageData(0, 0, width, height).data;
        data = { width, height, bits: 8, planes: splitChannels(rgba, count) };
        alpha = component(rgba, 3);
      }
      if (
        started !== revision ||
        viewer.world.getIndexOfItem(event.tiledImage) < 0 ||
        (await event.outdated())
      )
        return;
      await event.setData(rendered(data, settings, alpha), "context2d");
    } catch (error) {
      if (
        started === revision &&
        viewer.world.getIndexOfItem(event.tiledImage) >= 0 &&
        !(await event.outdated())
      )
        onError(`Could not adjust image colors: ${error}`);
    }
  };
  viewer.addHandler("tile-invalidated", handleTile);
  if (viewer.drawer.getType() !== "aperture-gpu")
    viewer.drawer.canvas.dataset.colorRenderer = "cpu";
  return {
    refresh() {
      if (viewer.drawer.getType() === "aperture-gpu") {
        (
          viewer.drawer as OpenSeadragon.DrawerBase & { refreshColors(): void }
        ).refreshColors();
        return;
      }
      viewer.drawer.canvas.dataset.colorRenderer = "cpu";
      revision++;
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        const started = revision;
        const items = Array.from(
          { length: viewer.world.getItemCount() },
          (_, i) => viewer.world.getItemAt(i),
        );
        // OSD reprocesses current tiles and evicts stale offscreen memory caches.
        // Its global timestamp also makes subsequently loaded tiles use this state.
        void viewer.requestInvalidate(true).catch((error) => {
          if (
            started === revision &&
            items.some((item) => viewer.world.getIndexOfItem(item) >= 0)
          )
            onError(`Could not refresh image colors: ${error}`);
        });
      });
    },
    wrapSource(
      source: ColorSource,
      image: ImageState,
      isPreview: boolean,
      baseUrl: string,
    ) {
      const count = image.channels?.length ?? 0;
      const wrapped = {
        ...source,
        apertureChannelCount: count,
        apertureChannelMode: image.channelMode ?? false,
      };
      if (!image.channelMode || !count) return wrapped;
      const bits = image.sampleBits === 16 ? 16 : 8;
      const extension = bits === 16 ? "u16" : "png";
      const getTileUrl = (level: number, x: number, y: number) =>
        `${baseUrl}/channels/0/${isPreview ? `preview.${extension}` : `tiles/${level}/${x}_${y}.${extension}`}`;
      return {
        ...wrapped,
        getTileUrl,
        hasTransparency: () => false,
        downloadTileStart(job: OpenSeadragon.ImageJob) {
          const controller = new AbortController();
          job.userData.controller = controller;
          void (async () => {
            await converterReady;
            const channels: Awaited<ReturnType<typeof channelPlane>>[] = [];
            // Two requests per OSD job keeps four concurrent tiles below the backend cap.
            for (let first = 0; first < count; first += 2) {
              channels.push(
                ...(await Promise.all(
                  Array.from(
                    { length: Math.min(2, count - first) },
                    (_, offset) =>
                      channelPlane(
                        job.src.replace(
                          "/channels/0/",
                          `/channels/${first + offset}/`,
                        ),
                        controller.signal,
                        bits,
                      ),
                  ),
                )),
              );
            }
            if (controller.signal.aborted) return;
            const { width, height } = channels[0];
            if (
              channels.some(
                (channel) =>
                  channel.width !== width || channel.height !== height,
              )
            )
              throw new Error("Channel tile dimensions differ.");
            const data: Planes = {
              width,
              height,
              bits,
              planes: channels.map((channel) => channel.plane),
            };
            if (isPreview) onPreviewPlanes?.(image.id, data);
            job.finish(data, null, planeType);
          })().catch((error) => {
            if (!controller.signal.aborted) {
              controller.abort();
              job.fail(String(error), null);
            }
          });
        },
        downloadTileAbort(job: OpenSeadragon.ImageJob) {
          job.userData.controller?.abort();
        },
      };
    },
  };
}
