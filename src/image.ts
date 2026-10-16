export type ImageState = {
  id: string;
  revision: number;
  name: string;
  path?: string;
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
  readOnly?: boolean;
  displayRanges?: [number, number][];
  calibration?: {
    pixelWidthUm: number | null;
    pixelHeightUm: number | null;
    zStepUm: number | null;
    timeIncrementSeconds: number | null;
  };
  dimensions?: {
    positions: string[];
    z: number;
    time: number;
    position: number;
    zIndex: number;
    timeIndex: number;
  };
};

export type DimensionAxis = "position" | "z" | "time" | "channel";

export function dimensionAxis(
  image: ImageState | null,
  axis: DimensionAxis,
  channel: number,
) {
  const dimensions = image?.dimensions;
  switch (axis) {
    case "position":
      return {
        index: dimensions?.position ?? 0,
        count: dimensions?.positions.length ?? 1,
        name: dimensions?.positions[dimensions.position] ?? "",
      };
    case "z":
      return {
        index: dimensions?.zIndex ?? 0,
        count: dimensions?.z ?? 1,
        name: "Z plane",
      };
    case "time":
      return {
        index: dimensions?.timeIndex ?? 0,
        count: dimensions?.time ?? 1,
        name: "Time point",
      };
    case "channel":
      return {
        index: channel + 1,
        count: (image?.channels?.length ?? 0) + 1,
        name:
          channel < 0 ? "Composite" : (image?.channels?.[channel]?.name ?? ""),
      };
  }
}

export type PlaneSelection = { position: number; z: number; time: number };

/** Serialize native plane changes, keeping only the latest requested coordinates. */
export class PlaneNavigation {
  selection: PlaneSelection | null = null;
  private confirmed: PlaneSelection | null = null;
  private pending = false;
  private generation = 0;

  private apply: (
    position: number,
    z: number,
    time: number,
  ) => Promise<boolean>;
  private changed: () => void;

  constructor(apply: PlaneNavigation["apply"], changed: () => void) {
    this.apply = apply;
    this.changed = changed;
  }

  setPlane(plane: PlaneSelection | null, preserve = false) {
    this.confirmed = plane;
    if (!preserve) {
      this.generation++;
      this.pending = false;
    }
    if (!this.pending) this.selection = plane;
  }

  select(axis: keyof PlaneSelection, value: number) {
    if (!this.selection) return;
    this.selection = { ...this.selection, [axis]: value };
    this.changed();
    void this.flush();
  }

  private async flush() {
    if (this.pending) return;
    this.pending = true;
    const generation = this.generation;
    while (
      this.selection &&
      (this.selection.position !== this.confirmed?.position ||
        this.selection.z !== this.confirmed?.z ||
        this.selection.time !== this.confirmed?.time)
    ) {
      const selected = this.selection;
      const applied = await this.apply(
        selected.position,
        selected.z,
        selected.time,
      );
      if (generation !== this.generation) return;
      if (!applied) {
        this.selection = this.confirmed;
        break;
      }
      this.confirmed = selected;
    }
    this.pending = false;
    this.changed();
  }
}

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
  private active = false;
  private pending = false;
  private disposed = false;
  private buffered = new Map<string, ImageState>();
  private listening: Promise<void>;
  private unlisten: (() => void) | undefined;
  private commands: {
    open: (path: string) => Promise<ImageState>;
    selectPlane?: (
      id: string,
      position: number,
      z: number,
      time: number,
    ) => Promise<ImageState>;
    state: () => Promise<ImageState | null>;
    cancel: () => Promise<void>;
    listen?: (changed: (state: ImageState) => void) => Promise<() => void>;
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
    this.listening = commands.listen
      ? commands
          .listen((state) => this.receive(state))
          .then(
            (unlisten) => {
              if (this.disposed) unlisten();
              else this.unlisten = unlisten;
            },
            (error) => {
              if (!this.disposed) this.failed(String(error));
            },
          )
      : Promise.resolve();
  }

  private invalidate() {
    clearTimeout(this.timer);
    return ++this.generation;
  }

  private begin(previous: ImageState | null = null) {
    const generation = this.invalidate();
    this.state = previous;
    this.active = true;
    this.pending = true;
    this.buffered.clear();
    return generation;
  }

  async open(path: string) {
    await this.request(() => this.commands.open(path));
  }

  async restore() {
    await this.request(() => this.commands.state());
  }

  async selectPlane(position: number, z: number, time: number) {
    const previous = this.state;
    if (!previous?.dimensions || !this.commands.selectPlane) return false;
    return this.request(
      () => this.commands.selectPlane!(previous.id, position, z, time),
      previous,
    );
  }

  private async request(
    command: () => Promise<ImageState | null>,
    previous: ImageState | null = null,
  ) {
    const generation = this.begin(previous);
    try {
      await this.listening;
      if (generation !== this.generation || this.disposed) return false;
      const state = await command();
      if (generation !== this.generation) return false;
      this.finish(state, generation);
      return Boolean(state);
    } catch (error) {
      if (generation === this.generation) {
        this.active = Boolean(previous);
        this.pending = false;
        this.buffered.clear();
        if (previous) {
          this.state = null;
          this.accept(previous, generation);
        }
        this.failed(String(error));
      }
      return false;
    }
  }

  private finish(state: ImageState | null, generation: number) {
    this.pending = false;
    if (state) {
      const buffered = this.buffered.get(state.id);
      this.accept(
        buffered && buffered.revision > state.revision ? buffered : state,
        generation,
      );
    } else this.active = false;
    this.buffered.clear();
  }

  private receive(state: ImageState) {
    if (this.disposed || !this.active) return;
    if (this.pending) {
      const previous = this.buffered.get(state.id);
      if (!previous || state.revision > previous.revision)
        this.buffered.set(state.id, state);
    } else if (state.id === this.state?.id) this.accept(state, this.generation);
  }

  private accept(state: ImageState, generation: number) {
    if (this.state?.id === state.id && state.revision <= this.state.revision)
      return;
    this.state = state;
    this.changed(state);
    this.schedulePoll(generation);
  }

  private schedulePoll(generation: number) {
    clearTimeout(this.timer);
    if (this.state?.phase === "loading") {
      const id = this.state.id;
      this.timer = setTimeout(() => void this.poll(generation, id), 250);
    }
  }

  private async poll(generation: number, id: string) {
    const revision = this.state?.revision;
    try {
      const state = await this.commands.state();
      if (generation !== this.generation) return;
      if (!state || state.id !== id) {
        if (this.state?.revision === revision) {
          this.failed("The image load is no longer available.");
          return;
        }
      } else this.accept(state, generation);
      this.schedulePoll(generation);
    } catch (error) {
      if (generation === this.generation && this.state?.revision === revision)
        this.failed(String(error));
    }
  }

  async cancel() {
    const selectingPlane = this.pending && Boolean(this.state?.dimensions);
    const generation = this.invalidate();
    this.active = false;
    this.pending = false;
    this.buffered.clear();
    try {
      await this.commands.cancel();
      if (generation !== this.generation) return false;
      // A plane command may have replaced the native id before its reply arrives.
      const state = selectingPlane
        ? ((await this.commands.state()) ?? this.state)
        : this.state;
      if (generation === this.generation && state) {
        this.accept(
          { ...state, revision: state.revision + 1, phase: "cancelled" },
          generation,
        );
      }
      return generation === this.generation;
    } catch (error) {
      if (generation === this.generation) this.failed(String(error));
      return false;
    }
  }

  dispose() {
    this.disposed = true;
    this.active = false;
    this.buffered.clear();
    this.unlisten?.();
    this.unlisten = undefined;
    this.invalidate();
  }
}
