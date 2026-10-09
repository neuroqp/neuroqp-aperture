export type ImageState = {
  id: string;
  name: string;
  width: number;
  height: number;
  phase: "loading" | "ready" | "cancelled" | "error";
  previewReady: boolean;
  tilesReady: boolean;
  progress: number | null;
  error: string | null;
  detail?: string;
  orientation?: number;
  channels?: { name: string; color: string }[];
  channelMode?: boolean;
};

// OSD flips stored pixels horizontally before applying clockwise rotation.
export function orientation(value = 1) {
  const degrees = [0, 0, 0, 180, 180, 270, 90, 90, 270][value] ?? 0;
  return { degrees, flipped: [2, 4, 5, 7].includes(value) };
}

export function tileFailure(
  currentUrl: string | null,
  phase: string | undefined,
  url: unknown,
) {
  if (
    !currentUrl ||
    !["loading", "ready"].includes(phase || "") ||
    typeof url !== "string" ||
    !url.startsWith(`${currentUrl}/`)
  )
    return null;
  return /\/preview\.(jpg|png)$/.test(url)
    ? "Could not display the image preview."
    : "Detail could not load. Preview remains available.";
}

export function pyramid(image: ImageState, url: (path: string) => string) {
  const size = Math.max(image.width, image.height);
  const maxLevel = Math.ceil(Math.log2(size));
  return {
    width: image.width,
    height: image.height,
    tileSize: 256,
    tileOverlap: 0,
    // Coarse foreground tiles must not blur the existing 1024-pixel preview.
    minLevel: maxLevel - Math.floor(Math.log2(Math.max(1, size / 1024))),
    maxLevel,
    getTileUrl: (level: number, x: number, y: number) =>
      `${url(image.id)}/tiles/${level}/${x}_${y}.jpg`,
  };
}

// Use original bounds so thumbnail rounding cannot shift the preview under tiles.
export function preview(image: ImageState, url: (path: string) => string) {
  return {
    width: image.width,
    height: image.height,
    tileSize: Math.max(image.width, image.height),
    tileOverlap: 0,
    minLevel: 0,
    maxLevel: 0,
    getTileUrl: () => `${url(image.id)}/preview.jpg`,
  };
}

/** Own one load; a newer open or cancellation invalidates every pending reply. */
export class ImageLoader {
  private generation = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private state: ImageState | null = null;
  private commands: {
    open: (path: string) => Promise<ImageState>;
    state: () => Promise<ImageState | null>;
    cancel: () => Promise<void>;
  };
  private changed: (state: ImageState) => void;
  private failed: (message: string) => void;

  constructor(
    commands: ImageLoader["commands"],
    changed: (state: ImageState) => void,
    failed: (message: string) => void,
  ) {
    this.commands = commands;
    this.changed = changed;
    this.failed = failed;
  }

  private invalidate() {
    clearTimeout(this.timer);
    return ++this.generation;
  }

  async open(path: string) {
    const generation = this.invalidate();
    this.state = null;
    try {
      const state = await this.commands.open(path);
      if (generation === this.generation) this.accept(state, generation);
    } catch (error) {
      if (generation === this.generation) this.failed(String(error));
    }
  }

  async restore() {
    const generation = this.generation;
    try {
      const state = await this.commands.state();
      if (state && generation === this.generation)
        this.accept(state, generation);
    } catch (error) {
      if (generation === this.generation) this.failed(String(error));
    }
  }

  private accept(state: ImageState, generation: number) {
    this.state = state;
    this.changed(state);
    if (state.phase === "loading") {
      this.timer = setTimeout(() => void this.poll(generation, state.id), 250);
    }
  }

  private async poll(generation: number, id: string) {
    try {
      const state = await this.commands.state();
      if (generation !== this.generation) return;
      if (!state || state.id !== id) {
        this.failed("The image load is no longer available.");
        return;
      }
      this.accept(state, generation);
    } catch (error) {
      if (generation === this.generation) this.failed(String(error));
    }
  }

  async cancel() {
    const generation = this.invalidate();
    try {
      await this.commands.cancel();
      if (generation === this.generation && this.state) {
        this.accept({ ...this.state, phase: "cancelled" }, generation);
      }
      return generation === this.generation;
    } catch (error) {
      if (generation === this.generation) this.failed(String(error));
      return false;
    }
  }

  dispose() {
    this.invalidate();
  }
}
