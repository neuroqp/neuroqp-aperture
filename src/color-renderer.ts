import type OpenSeadragon from "openseadragon";
import type { ColorSettings } from "./color-settings";
import { copyColorSettings } from "./color-settings.ts";
import type { ImageState } from "./image";

export const planeType = "aperturePlanes";
export type Planes = { width: number; height: number; planes: Uint8Array[] };
type ColorSource = OpenSeadragon.TileSourceOptions & {
  getTileUrl: (level: number, x: number, y: number) => string;
  apertureChannelCount?: number;
  apertureChannelMode?: boolean;
};

/** Routes add after their own transfer function, then clamp once at output. */
export function composeColorPixels(
  planes: readonly Uint8Array[],
  settings: ColorSettings,
  alpha?: Uint8Array,
) {
  const size = planes[0]?.length ?? 0;
  if (planes.some((plane) => plane.length !== size))
    throw new Error("Channel tile dimensions differ.");
  const lookup = colorLookup(planes.length, settings);
  const output = new Uint8ClampedArray(size * 4);
  for (let pixel = 0; pixel < size; pixel++) {
    let red = 0,
      green = 0,
      blue = 0;
    for (let channel = 0; channel < planes.length; channel++) {
      const at = planes[channel][pixel] * 3;
      const table = lookup[channel];
      red += table[at];
      green += table[at + 1];
      blue += table[at + 2];
    }
    const at = pixel * 4;
    output[at] = red;
    output[at + 1] = green;
    output[at + 2] = blue;
    output[at + 3] = alpha?.[pixel] ?? 255;
  }
  return output;
}

/** Keep route clipping ahead of summation, including values above 255. */
export function colorLookup(count: number, settings: ColorSettings) {
  const lookup = Array.from({ length: count }, () => new Float64Array(256 * 3));
  for (const route of settings.connections) {
    const table = lookup[Number(route.source)];
    const hex = settings.colors[route.color];
    if (!table || !/^#[0-9a-f]{6}$/i.test(hex ?? "")) continue;
    const color = [1, 3, 5].map((start) =>
      parseInt(hex.slice(start, start + 2), 16),
    );
    for (let value = 0; value < 256; value++) {
      const intensity = Math.max(
        0,
        Math.min(
          1,
          (value / 255 - 0.5) * (route.contrast / 100) +
            0.5 +
            route.brightness / 100,
        ),
      );
      for (let component = 0; component < 3; component++)
        table[value * 3 + component] += intensity * color[component];
    }
  }
  return lookup;
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
        routes[0].brightness === 0 &&
        routes[0].contrast === 100
      );
    })
  );
}

async function channelPlane(url: string, signal: AbortSignal) {
  const response = await fetch(url, { signal });
  if (!response.ok)
    throw new Error(`Channel tile failed (${response.status}).`);
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
      plane: Uint8Array.from(
        { length: rgba.length / 4 },
        (_, i) => rgba[i * 4],
      ),
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
  let disposed = false;
  const handleTile = async (event: OpenSeadragon.TileInvalidatedEvent) => {
    if (viewer.drawer.getType() === "aperture-gpu") return;
    const source = event.tiledImage.source as OpenSeadragon.TileSource &
      ColorSource;
    const count = source.apertureChannelCount;
    if (!count || disposed || viewer.world.getIndexOfItem(event.tiledImage) < 0)
      return;
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
        const size = width * height;
        data = {
          width,
          height,
          planes: Array.from({ length: count }, (_, channel) =>
            Uint8Array.from(
              { length: size },
              (_, pixel) => rgba[pixel * 4 + channel],
            ),
          ),
        };
        alpha = Uint8Array.from(
          { length: size },
          (_, pixel) => rgba[pixel * 4 + 3],
        );
      }
      if (
        disposed ||
        started !== revision ||
        viewer.world.getIndexOfItem(event.tiledImage) < 0 ||
        (await event.outdated())
      )
        return;
      await event.setData(rendered(data, settings, alpha), "context2d");
    } catch (error) {
      if (
        !disposed &&
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
      if (frame || disposed) return;
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
            !disposed &&
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
      const getTileUrl = (level: number, x: number, y: number) =>
        `${baseUrl}/channels/0/${isPreview ? "preview.png" : `tiles/${level}/${x}_${y}.png`}`;
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
            job.finish(
              {
                width,
                height,
                planes: channels.map((channel) => channel.plane),
              },
              null,
              planeType,
            );
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
    dispose() {
      disposed = true;
      cancelAnimationFrame(frame);
      viewer.removeHandler("tile-invalidated", handleTile);
    },
  };
}
