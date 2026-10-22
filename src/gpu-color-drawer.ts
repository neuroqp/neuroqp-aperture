import type OpenSeadragon from "openseadragon";
import type { ColorSettings } from "./color-settings";
import { colorRoutes, planeType, type Planes } from "./color-renderer.ts";

const vertex = `#version 300 es
in vec2 position;
uniform mat3 transform;
uniform vec4 textureBounds;
out vec2 uv;
void main() {
  gl_Position = vec4((transform * vec3(position, 1.0)).xy, 0.0, 1.0);
  uv = mix(textureBounds.xy, textureBounds.zw, position);
}`;
// Channel tiles hold integer source samples; ordinary RGB(A)8 tiles stay normalized.
const fragment = (integer: boolean) => {
  const sampler = integer ? "usampler2DArray" : "sampler2DArray";
  return `#version 300 es
precision highp float;
precision highp int;
precision highp ${sampler};
precision highp sampler2D;
uniform ${sampler} samples;
uniform sampler2D routes;
uniform int routeCount;
uniform int channels;
uniform float opacity;
in vec2 uv;
out vec4 pixel;
// Route i: texel 2i holds channel, minimum, maximum and brightness; 2i+1 its color.
vec4 route(int texel) {
  int width = textureSize(routes, 0).x;
  return texelFetch(routes, ivec2(texel % width, texel / width), 0);
}
void main() {
  ivec2 size = textureSize(samples, 0).xy;
  ivec2 coordinate = clamp(ivec2(uv * vec2(size)), ivec2(0), size - 1);
  ${integer ? "" : "vec4 original = texelFetch(samples, ivec3(coordinate, 0), 0);"}
  vec3 sum = vec3(0.0);
  for (int i = 0; i < routeCount; i++) {
    vec4 range = route(2 * i);
    int channel = int(range.x);
    if (channel >= channels) continue;
    float value = ${integer ? "float(texelFetch(samples, ivec3(coordinate, channel), 0).r)" : "round(original[channel] * 255.0)"};
    sum += clamp((value - range.y) / (range.z - range.y), 0.0, 1.0) * range.w *
      route(2 * i + 1).rgb;
  }
  float alpha = opacity${integer ? "" : " * original.a"};
  // Match the 8-bit CPU composite.
  vec3 color = roundEven(clamp(sum, 0.0, 1.0) * 255.0) / 255.0;
  pixel = vec4(color * alpha, alpha);
}`;
};

type TextureTile = {
  texture: WebGLTexture;
  width: number;
  height: number;
  channels: number;
  rgb: boolean;
  alive: boolean;
};
type DrawPacket = {
  data: TextureTile;
  transform: number[];
  bounds: number[];
  opacity: number;
};
type Program = {
  program: WebGLProgram;
  uniforms: Record<string, WebGLUniformLocation | null>;
};

/**
 * Pack two RGBA32F texels per route, row-major in rows of `width` texels:
 * (channel, minimum, maximum, brightness) then (red, green, blue, 0).
 */
export function routeTexture(settings: ColorSettings, width: number) {
  const routes = colorRoutes(settings, 32);
  const height = Math.max(1, Math.ceil((routes.length * 2) / width));
  const data = new Float32Array(width * height * 4);
  routes.forEach((route, index) => {
    const { channel, minimum, maximum, brightness, color } = route;
    data.set([channel, minimum, maximum, brightness / 100], index * 8);
    data.set(
      color.map((component) => component / 255),
      index * 8 + 4,
    );
  });
  return { count: routes.length, width, height, data };
}

function program(gl: WebGL2RenderingContext, integer: boolean) {
  const shaders = [
    [gl.VERTEX_SHADER, vertex],
    [gl.FRAGMENT_SHADER, fragment(integer)],
  ].map(([kind, source]) => {
    const shader = gl.createShader(kind as number)!;
    gl.shaderSource(shader, source as string);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const error = gl.getShaderInfoLog(shader);
      gl.deleteShader(shader);
      throw new Error(`GPU color shader failed: ${error}`);
    }
    return shader;
  });
  const result = gl.createProgram()!;
  for (const shader of shaders) gl.attachShader(result, shader);
  // Both programs share the quad buffer's attribute location.
  gl.bindAttribLocation(result, 0, "position");
  gl.linkProgram(result);
  for (const shader of shaders) gl.deleteShader(shader);
  if (!gl.getProgramParameter(result, gl.LINK_STATUS)) {
    const error = gl.getProgramInfoLog(result);
    gl.deleteProgram(result);
    throw new Error(`GPU color program failed: ${error}`);
  }
  return result;
}

/** OSD owns tile lifetimes; settings redraw GPU samples without invalidating them. */
export function createGpuColorDrawer(
  osd: typeof OpenSeadragon,
  getSettings: () => ColorSettings,
  onError: (message: string) => void,
): OpenSeadragon.DrawerConstructor {
  return class GpuColorDrawer extends osd.DrawerBase {
    private gl!: WebGL2RenderingContext;
    private programs: Program[] = [];
    private buffer!: WebGLBuffer;
    private routes: WebGLTexture | null = null;
    private routeRows = 0;
    private routeCount = 0;
    private dirty = true;
    private revision = 0;
    private fallbackFrame = 0;
    private lost = false;
    private packets: DrawPacket[] | null = null;
    private replayPending = false;

    get defaultOptions() {
      return { usePrivateCache: true, preloadCache: false };
    }

    static isSupported() {
      const canvas = document.createElement("canvas");
      const gl = canvas.getContext("webgl2");
      if (!gl) return false;
      let supported = false;
      try {
        for (const integer of [true, false])
          gl.deleteProgram(program(gl, integer));
        supported = true;
      } catch (error) {
        console.warn(error);
      } finally {
        gl.getExtension("WEBGL_lose_context")?.loseContext();
      }
      return supported;
    }

    constructor(options: OpenSeadragon.DrawerConstructorParameters) {
      super(options);
      try {
        this.gl = this.canvas.getContext("webgl2", {
          alpha: true,
          antialias: false,
          premultipliedAlpha: true,
          depth: false,
          stencil: false,
        })!;
        const gl = this.gl;
        if (!gl) throw new Error("WebGL2 context is unavailable.");
        // programs[0] samples integer channel tiles, programs[1] RGB(A)8 tiles.
        for (const integer of [true, false]) {
          const shader = program(gl, integer);
          this.programs.push({
            program: shader,
            uniforms: Object.fromEntries(
              [
                "transform",
                "textureBounds",
                "routeCount",
                "channels",
                "opacity",
              ].map((name) => [name, gl.getUniformLocation(shader, name)]),
            ),
          });
          gl.useProgram(shader);
          gl.uniform1i(gl.getUniformLocation(shader, "samples"), 0);
          gl.uniform1i(gl.getUniformLocation(shader, "routes"), 1);
        }
        this.buffer = gl.createBuffer()!;
        if (!this.buffer) throw new Error("GPU resource allocation failed.");
        gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
        gl.bufferData(
          gl.ARRAY_BUFFER,
          new Float32Array([0, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 1]),
          gl.STATIC_DRAW,
        );
        gl.enableVertexAttribArray(0);
        gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
        gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
        gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
        gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
        this.checkUpload();
        this.canvas.dataset.colorRenderer = "webgl2";
        this.canvas.addEventListener("webglcontextlost", this.contextLost);
      } catch (error) {
        this.fallback(`Could not initialize GPU color rendering: ${error}`);
      }
    }

    getType() {
      return "aperture-gpu";
    }
    getSupportedDataFormats() {
      return [planeType, "image", "context2d"];
    }
    canRotate() {
      return true;
    }
    setImageSmoothingEnabled() {
      /* Display samples intentionally use nearest-neighbor. */
    }
    _createDrawingElement() {
      return document.createElement("canvas");
    }

    private textureParameters(target: number) {
      const gl = this.gl;
      gl.texParameteri(target, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(target, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(target, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(target, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    }

    internalCacheCreate(
      cache: OpenSeadragon.CacheRecord,
      tile: OpenSeadragon.Tile,
    ): TextureTile | undefined {
      if (this.lost) return undefined;
      const gl = this.gl;
      const texture = gl.createTexture();
      if (!texture) {
        this.fallback("GPU tile texture allocation failed.");
        return undefined;
      }
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D_ARRAY, texture);
      this.textureParameters(gl.TEXTURE_2D_ARRAY);
      try {
        if (cache.type === planeType) {
          const data = cache.data as Planes;
          const wide = data.bits === 16;
          if (
            !data.planes.length ||
            data.planes.length > 32 ||
            data.planes.some(
              (plane) =>
                plane.length !== data.width * data.height ||
                !(plane instanceof (wide ? Uint16Array : Uint8Array)),
            )
          )
            throw new Error(
              "Channel tile dimensions differ or channel count exceeds 32.",
            );
          gl.texStorage3D(
            gl.TEXTURE_2D_ARRAY,
            1,
            wide ? gl.R16UI : gl.R8UI,
            data.width,
            data.height,
            data.planes.length,
          );
          data.planes.forEach((plane, channel) =>
            gl.texSubImage3D(
              gl.TEXTURE_2D_ARRAY,
              0,
              0,
              0,
              channel,
              data.width,
              data.height,
              1,
              gl.RED_INTEGER,
              wide ? gl.UNSIGNED_SHORT : gl.UNSIGNED_BYTE,
              plane,
            ),
          );
          this.checkUpload();
          return {
            texture,
            width: data.width,
            height: data.height,
            channels: data.planes.length,
            rgb: false,
            alive: true,
          };
        }
        const data =
          cache.type === "context2d" ? cache.data.canvas : cache.data;
        const width = data.naturalWidth || data.width;
        const height = data.naturalHeight || data.height;
        gl.texStorage3D(gl.TEXTURE_2D_ARRAY, 1, gl.RGBA8, width, height, 1);
        gl.texSubImage3D(
          gl.TEXTURE_2D_ARRAY,
          0,
          0,
          0,
          0,
          width,
          height,
          1,
          gl.RGBA,
          gl.UNSIGNED_BYTE,
          data,
        );
        this.checkUpload();
        const source = tile.tiledImage?.source as OpenSeadragon.TileSource & {
          apertureChannelCount?: number;
        };
        return {
          texture,
          width,
          height,
          channels: Math.min(3, source.apertureChannelCount || 3),
          rgb: true,
          alive: true,
        };
      } catch (error) {
        gl.deleteTexture(texture);
        this.fallback(`Could not upload GPU color samples: ${error}`);
        return undefined;
      }
    }

    private checkUpload() {
      const error = this.gl.getError();
      if (error !== this.gl.NO_ERROR) throw new Error(`WebGL error ${error}`);
    }

    internalCacheFree(data: TextureTile | undefined) {
      if (data?.texture) {
        data.alive = false;
        this.gl.deleteTexture(data.texture);
      }
    }

    refreshColors() {
      this.dirty = true;
      this.revision++;
      if (this.replayPending || this.lost) return;
      this.replayPending = true;
      queueMicrotask(() => {
        this.replayPending = false;
        if (this.lost) return;
        if (!this.packets || this.packets.some(({ data }) => !data.alive)) {
          this.viewer.forceRedraw();
          return;
        }
        // Replay the last displayed geometry. OSD's own animation/loading frames
        // replace it normally; color edits never advance tile blending themselves.
        this.renderPackets();
      });
    }

    draw(items: OpenSeadragon.TiledImage[]) {
      if (this.lost) return;
      const size = this.viewport.getContainerSize();
      const width = Math.round(size.x * osd.pixelDensityRatio);
      const height = Math.round(size.y * osd.pixelDensityRatio);
      if (this.canvas.width !== width || this.canvas.height !== height) {
        this.canvas.width = width;
        this.canvas.height = height;
      }
      const packets: DrawPacket[] = [];
      const bounds = this.viewport.getBoundsNoRotateWithMargins(true);
      const center = bounds.getCenter();
      const view = osd.Mat3.makeScaling(
        (this.viewport.getFlip() ? -2 : 2) / bounds.width,
        -2 / bounds.height,
      )
        .multiply(
          osd.Mat3.makeRotation(
            (-this.viewport.getRotation(true) * Math.PI) / 180,
          ),
        )
        .multiply(osd.Mat3.makeTranslation(-center.x, -center.y));
      for (const item of items) {
        if (!item.getOpacity()) continue;
        let transform = view;
        const rotation = item.getRotation(true);
        if (rotation % 360) {
          const center = item.getBoundsNoRotate(true).getCenter();
          transform = view.multiply(
            osd.Mat3.makeTranslation(center.x, center.y)
              .multiply(osd.Mat3.makeRotation((-rotation * Math.PI) / 180))
              .multiply(osd.Mat3.makeTranslation(-center.x, -center.y)),
          );
        }
        // OSD orders coarse/preview first and sharper tiles last. Color addition
        // happens within one tile; tiles themselves always blend source-over.
        for (const { tile } of item.getTilesToDraw()) {
          const data = this.getDataToDraw(tile) as TextureTile | undefined;
          if (!data) continue;
          const rect = tile.positionedBounds;
          const overlap = item.source.tileOverlap || 0;
          const xFraction = overlap / data.width;
          const yFraction = overlap / data.height;
          const left = tile.x ? xFraction : 0;
          const top = tile.y ? yFraction : 0;
          const right = tile.isRightMost ? 1 : 1 - xFraction;
          const bottom = tile.isBottomMost ? 1 : 1 - yFraction;
          const x = rect.x + left * rect.width;
          const y = rect.y + top * rect.height;
          const w = rect.width * (right - left);
          const h = rect.height * (bottom - top);
          const model = new osd.Mat3([w, 0, 0, 0, h, 0, x, y, 1]);
          if (tile.flipped) model.scaleAndTranslateSelf(-1, 1, 1, 0);
          model.scaleAndTranslateOtherSetSelf(transform);
          const sourceWidth = tile.sourceBounds
            ? Math.min(tile.sourceBounds.width, data.width) / data.width
            : 1;
          const sourceHeight = tile.sourceBounds
            ? Math.min(tile.sourceBounds.height, data.height) / data.height
            : 1;
          packets.push({
            data,
            transform: model.values,
            bounds: [
              left * sourceWidth,
              top * sourceHeight,
              right * sourceWidth,
              bottom * sourceHeight,
            ],
            opacity: (tile.opacity ?? 1) * item.getOpacity(),
          });
        }
      }
      this.packets = packets;
      this.renderPackets();
    }

    private renderPackets() {
      const gl = this.gl;
      gl.viewport(0, 0, this.canvas.width, this.canvas.height);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.activeTexture(gl.TEXTURE1);
      gl.bindTexture(gl.TEXTURE_2D, this.routes);
      if (this.dirty) {
        try {
          this.uploadRoutes();
        } catch (error) {
          this.fallback(`Could not upload GPU color routes: ${error}`);
          return;
        }
        this.dirty = false;
      }
      gl.activeTexture(gl.TEXTURE0);
      let active: Program | undefined;
      for (const { data, transform, bounds, opacity } of this.packets!) {
        const next = this.programs[data.rgb ? 1 : 0];
        if (next !== active) {
          active = next;
          gl.useProgram(active.program);
          gl.uniform1i(active.uniforms.routeCount, this.routeCount);
        }
        gl.uniformMatrix3fv(active.uniforms.transform, false, transform);
        gl.uniform4fv(active.uniforms.textureBounds, bounds);
        gl.uniform1i(active.uniforms.channels, data.channels);
        gl.uniform1f(active.uniforms.opacity, opacity);
        gl.bindTexture(gl.TEXTURE_2D_ARRAY, data.texture);
        gl.drawArrays(gl.TRIANGLES, 0, 6);
      }
      this.canvas.dataset.colorRevision = String(this.revision);
      this.canvas.dispatchEvent(
        new CustomEvent("aperture-color-draw", {
          detail: { revision: this.revision, mode: "webgl2" },
        }),
      );
    }

    /** Expects TEXTURE1 active; storage grows only with the route count. */
    private uploadRoutes() {
      const gl = this.gl;
      const packed = routeTexture(
        getSettings(),
        Math.min(1024, gl.getParameter(gl.MAX_TEXTURE_SIZE)),
      );
      if (!this.routes || packed.height > this.routeRows) {
        if (this.routes) gl.deleteTexture(this.routes);
        this.routes = gl.createTexture();
        if (!this.routes) throw new Error("texture allocation failed");
        gl.bindTexture(gl.TEXTURE_2D, this.routes);
        this.textureParameters(gl.TEXTURE_2D);
        gl.texStorage2D(
          gl.TEXTURE_2D,
          1,
          gl.RGBA32F,
          packed.width,
          packed.height,
        );
        this.routeRows = packed.height;
      }
      gl.texSubImage2D(
        gl.TEXTURE_2D,
        0,
        0,
        0,
        packed.width,
        packed.height,
        gl.RGBA,
        gl.FLOAT,
        packed.data,
      );
      this.checkUpload();
      this.routeCount = packed.count;
    }

    private contextLost = (event: Event) => {
      event.preventDefault();
      this.canvas.dataset.colorRenderer = "webgl2-lost";
      this.fallback();
    };

    private fallback(message?: string) {
      if (this.lost) return;
      this.lost = true;
      if (message) console.warn(message);
      // Retain OSD's original cache data and let the existing CPU binding
      // recompose it. Context loss must not require decoding the source again.
      this.fallbackFrame = requestAnimationFrame(() => {
        this.fallbackFrame = 0;
        this.viewer.requestDrawer("canvas", { drawerOptions: {} });
        this.viewer.drawer.canvas.dataset.colorRenderer = "cpu";
        void this.viewer
          .requestInvalidate(true)
          .catch((error) =>
            onError(`Could not recover color rendering: ${error}`),
          );
      });
    }

    destroy() {
      this.lost = true;
      this.packets = null;
      if (this.fallbackFrame) cancelAnimationFrame(this.fallbackFrame);
      this.canvas.removeEventListener("webglcontextlost", this.contextLost);
      this.destroyInternalCache();
      if (this.gl) {
        if (this.routes) this.gl.deleteTexture(this.routes);
        if (this.buffer) this.gl.deleteBuffer(this.buffer);
        for (const { program } of this.programs) this.gl.deleteProgram(program);
      }
      this.canvas.remove();
      super.destroy();
    }
  };
}
