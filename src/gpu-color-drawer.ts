import type OpenSeadragon from "openseadragon";
import type { ColorSettings } from "./color-settings";
import { colorLookup, planeType, type Planes } from "./color-renderer.ts";

const vertex = `#version 300 es
in vec2 position;
uniform mat3 transform;
uniform vec4 textureBounds;
out vec2 uv;
void main() {
  gl_Position = vec4((transform * vec3(position, 1.0)).xy, 0.0, 1.0);
  uv = mix(textureBounds.xy, textureBounds.zw, position);
}`;
const fragment = `#version 300 es
precision highp float;
precision highp sampler2DArray;
precision highp sampler2D;
uniform sampler2DArray samples;
uniform sampler2D lookup;
uniform int channels;
uniform bool rgb;
uniform float opacity;
in vec2 uv;
out vec4 pixel;
void main() {
  ivec2 size = textureSize(samples, 0).xy;
  ivec2 coordinate = clamp(ivec2(uv * vec2(size)), ivec2(0), size - 1);
  vec3 sum = vec3(0.0);
  float alpha = opacity;
  if (rgb) {
    vec4 original = texelFetch(samples, ivec3(coordinate, 0), 0);
    for (int channel = 0; channel < 3; channel++) {
      if (channel >= channels) break;
      int value = int(round(original[channel] * 255.0));
      sum += texelFetch(lookup, ivec2(value, channel), 0).rgb;
    }
    alpha *= original.a;
  } else {
    for (int channel = 0; channel < channels; channel++) {
      float sampleValue = texelFetch(samples, ivec3(coordinate, channel), 0).r;
      int value = int(round(sampleValue * 255.0));
      sum += texelFetch(lookup, ivec2(value, channel), 0).rgb;
    }
  }
  // Match the 8-bit CPU composite before display range (a CSS filter).
  vec3 color = roundEven(clamp(sum, 0.0, 1.0) * 255.0) / 255.0;
  pixel = vec4(color * alpha, alpha);
}`;

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

/** Float lookups preserve each route's clamp and clamp only the final sum. */
export function gpuColorLookup(settings: ColorSettings, count = 32) {
  const result = new Float32Array(256 * count * 4);
  colorLookup(count, settings).forEach((table, channel) => {
    for (let value = 0; value < 256; value++) {
      const at = (channel * 256 + value) * 4;
      for (let component = 0; component < 3; component++)
        result[at + component] = table[value * 3 + component] / 255;
    }
  });
  return result;
}

function program(gl: WebGL2RenderingContext) {
  const shaders = [
    [gl.VERTEX_SHADER, vertex],
    [gl.FRAGMENT_SHADER, fragment],
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
    private shader!: WebGLProgram;
    private buffer!: WebGLBuffer;
    private lookup!: WebGLTexture;
    private uniforms!: Record<string, WebGLUniformLocation | null>;
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
        const shader = program(gl);
        gl.deleteProgram(shader);
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
        this.shader = program(gl);
        this.buffer = gl.createBuffer()!;
        this.lookup = gl.createTexture()!;
        if (!this.buffer || !this.lookup)
          throw new Error("GPU resource allocation failed.");
        this.uniforms = Object.fromEntries(
          [
            "transform",
            "textureBounds",
            "samples",
            "lookup",
            "channels",
            "rgb",
            "opacity",
          ].map((name) => [name, gl.getUniformLocation(this.shader, name)]),
        );
        gl.useProgram(this.shader);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
        gl.bufferData(
          gl.ARRAY_BUFFER,
          new Float32Array([0, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 1]),
          gl.STATIC_DRAW,
        );
        const position = gl.getAttribLocation(this.shader, "position");
        gl.enableVertexAttribArray(position);
        gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
        gl.uniform1i(this.uniforms.samples, 0);
        gl.uniform1i(this.uniforms.lookup, 1);
        gl.activeTexture(gl.TEXTURE1);
        gl.bindTexture(gl.TEXTURE_2D, this.lookup);
        this.textureParameters(gl.TEXTURE_2D);
        gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA32F, 256, 32);
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
          if (
            !data.planes.length ||
            data.planes.length > 32 ||
            data.planes.some(
              (plane) => plane.length !== data.width * data.height,
            )
          )
            throw new Error(
              "Channel tile dimensions differ or channel count exceeds 32.",
            );
          gl.texStorage3D(
            gl.TEXTURE_2D_ARRAY,
            1,
            gl.R8,
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
              gl.RED,
              gl.UNSIGNED_BYTE,
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
          channels: source.apertureChannelCount || 3,
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
      gl.useProgram(this.shader);
      gl.activeTexture(gl.TEXTURE1);
      gl.bindTexture(gl.TEXTURE_2D, this.lookup);
      if (this.dirty) {
        gl.texSubImage2D(
          gl.TEXTURE_2D,
          0,
          0,
          0,
          256,
          32,
          gl.RGBA,
          gl.FLOAT,
          gpuColorLookup(getSettings()),
        );
        this.dirty = false;
      }
      gl.activeTexture(gl.TEXTURE0);
      for (const { data, transform, bounds, opacity } of this.packets!) {
        gl.uniformMatrix3fv(this.uniforms.transform, false, transform);
        gl.uniform4fv(this.uniforms.textureBounds, bounds);
        gl.uniform1i(this.uniforms.channels, data.channels);
        gl.uniform1i(this.uniforms.rgb, data.rgb ? 1 : 0);
        gl.uniform1f(this.uniforms.opacity, opacity);
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
        if (this.lookup) this.gl.deleteTexture(this.lookup);
        if (this.buffer) this.gl.deleteBuffer(this.buffer);
        if (this.shader) this.gl.deleteProgram(this.shader);
      }
      this.canvas.remove();
      super.destroy();
    }
  };
}
