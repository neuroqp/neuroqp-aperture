#!/usr/bin/env python3
"""Benchmark an already-open native Tauri image, without opening or decoding it.

Inside one scripts/headless-session --driver 4444, open an existing pyramid and
keep the app open, then benchmark it:
  python3 scripts/native-smoke.py BINARY PYRAMID --leave-open --output artifacts/color.json
  python3 scripts/native-color-benchmark.py --session artifacts/color.session.json

Inputs follow the real color-panel input handler at event-loop intervals. Timing
does not read pixels. A separate correctness pass checks framebuffer changes.
The next animation frame is a presentation opportunity, not proof of physical
screen presentation. Optional WebGL2 fences measure completion, not GPU duration.
"""

import argparse
import json
import math
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

import compositor


SCRIPT = r"""
const config = arguments[0], done = arguments[arguments.length - 1];
(async () => {
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const focusedAtStart = document.hasFocus();
  const canvases = [...document.querySelectorAll('#viewer canvas')];
  const canvas = canvases.sort((a,b) => b.width*b.height-a.width*a.height)[0];
  if (!canvas) throw new Error('No native viewer canvas');
  if (document.querySelector('#viewer').dataset.phase !== 'ready')
    throw new Error('Open an image and wait for its pyramid before measuring');
  if (document.querySelector('#color-panel').hidden)
    document.querySelector('#color-button')?.click();
  if (!document.querySelector(config.selector))
    document.querySelector('[data-edge]')?.dispatchEvent(
      new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
  const input = () => document.querySelector(config.selector);
  if (!input() || input().disabled) throw new Error('Enabled color slider not found');
  if (config.values.some(value => !Number.isFinite(value) ||
      value < Number(input().min) || value > Number(input().max)) ||
      config.values[0] === config.values[1])
    throw new Error('Choose two different values within the slider range');
  const originalValue = input().value;
  const viewer = window.__apertureViewer;
  const rect = value => ({x:value.x,y:value.y,width:value.width,height:value.height});
  const geometry = () => viewer ? {
    bounds:rect(viewer.viewport.getBoundsNoRotate(true)),
    bounds_with_margins:rect(viewer.viewport.getBoundsNoRotateWithMargins(true)),
    zoom:viewer.viewport.getZoom(true),rotation:viewer.viewport.getRotation(true),
    center:{x:viewer.viewport.getCenter(true).x,y:viewer.viewport.getCenter(true).y},
    container:[viewer.viewport.getContainerSize().x,viewer.viewport.getContainerSize().y],
    items:Array.from({length:viewer.world.getItemCount()},(_,i) => {
      const item = viewer.world.getItemAt(i);
      return {bounds:rect(item.getBoundsNoRotate(true)),opacity:item.getOpacity(),
              visible_tiles:item.getTilesToDraw().length};
    }),
  } : null;
  const geometryAtStart = geometry();
  const originals = [], glContexts = new Set();
  let trial = null, composed = 0, drawSerial = 0, fetched = 0, running = true;
  let readbacks = 0;
  const uploads = [], resources = [];
  const observer = new PerformanceObserver(list => resources.push(...list.getEntries()));
  observer.observe({entryTypes:['resource']});
  originals.push(() => observer.disconnect());
  const frameIntervals = [], gpuInfo = [];
  let previousFrame = null;
  function frames(now) {
    if (!running) return;
    if (previousFrame !== null) frameIntervals.push(now-previousFrame);
    previousFrame = now;
    requestAnimationFrame(frames);
  }
  requestAnimationFrame(frames);
  function wrap(proto, name, callback) {
    const old = proto[name];
    proto[name] = function(...args) {
      const result = old.apply(this,args);
      callback(this,args);
      return result;
    };
    originals.push(() => { proto[name] = old; });
  }
  const mainCanvas = c => c === canvas;
  function submitted(context) {
    if (!trial || !mainCanvas(context.canvas)) return;
    // CPU: only count viewport draws after this input produced a color tile.
    // GPU: a draw event with an updated revision confirms the settings used.
    if (!trial.valid) return;
    if (trial.nextFrame) return;
    const now = performance.now();
    trial.firstSubmission ??= now;
    trial.lastSubmission = now;
    trial.context = context;
    trial.drawSerial = ++drawSerial;
    if (!trial.framePending) {
      const current = trial;
      current.framePending = true;
      requestAnimationFrame(() => { current.nextFrame = performance.now(); });
    }
  }
  wrap(CanvasRenderingContext2D.prototype,'putImageData', context => {
    if (!trial) return;
    composed++;
    trial.coloredCanvases.add(context.canvas);
    trial.firstComposition ??= performance.now();
    if (mainCanvas(context.canvas)) { trial.valid = true; submitted(context); }
  });
  wrap(CanvasRenderingContext2D.prototype,'drawImage', (context,args) => {
    if (!trial || !trial.coloredCanvases.has(args[0])) return;
    // Follow newly colored tile pixels through OSD's optional sketch canvas.
    trial.coloredCanvases.add(context.canvas);
    if (mainCanvas(context.canvas)) { trial.valid = true; submitted(context); }
  });
  wrap(CanvasRenderingContext2D.prototype,'getImageData', () => { readbacks++; });
  for (const proto of [window.WebGLRenderingContext?.prototype,
                       window.WebGL2RenderingContext?.prototype].filter(Boolean)) {
    for (const name of ['drawArrays','drawElements']) {
      if (!Object.prototype.hasOwnProperty.call(proto,name) &&
          proto === window.WebGL2RenderingContext?.prototype) continue;
      wrap(proto,name, context => {
        glContexts.add(context);
        if (mainCanvas(context.canvas) && trial) trial.context = context;
      });
    }
    for (const name of ['texImage2D','texSubImage2D','texImage3D','texSubImage3D',
                       'texStorage2D','texStorage3D']) {
      if (!Object.prototype.hasOwnProperty.call(proto,name)) continue;
      wrap(proto,name, (context,args) => {
        glContexts.add(context);
        const at = name.startsWith('texStorage') || name.startsWith('texImage') ? 3 :
                   name === 'texSubImage2D' ? 4 : 5;
        uploads.push({method:name,target:args[0],
          width:typeof args[at]==='number'?args[at]:null,
          height:typeof args[at+1]==='number'?args[at+1]:null,
          depth:name.endsWith('3D')?args[at+2]:null});
      });
    }
  }
  const oldFetch = window.fetch;
  window.fetch = function(...args) { fetched++; return oldFetch.apply(this,args); };
  originals.push(() => { window.fetch = oldFetch; });
  function snapshot(context) {
    const w = canvas.width, h = canvas.height;
    let pixels;
    if (context instanceof CanvasRenderingContext2D)
      pixels = context.getImageData(0,0,w,h).data;
    else {
      pixels = new Uint8Array(w*h*4);
      context.readPixels(0,0,w,h,context.RGBA,context.UNSIGNED_BYTE,pixels);
      const error = context.getError();
      if (error !== context.NO_ERROR) throw new Error('Framebuffer read failed: '+error);
    }
    let hash = 2166136261, rgb = [0,0,0], nonblack = 0;
    // Sample the same grid in both passes, rather than hash a multi-MB buffer.
    for (let y=0; y<h; y+=13) for (let x=0; x<w; x+=13) {
      // WebGL readPixels starts at the bottom; 2D starts at the top.
      const row = context instanceof CanvasRenderingContext2D ? y : h-1-y;
      const at = (row*w+x)*4;
      for (let c=0;c<3;c++) {
        hash = Math.imul(hash ^ pixels[at+c],16777619) >>> 0;
        rgb[c] += pixels[at+c];
      }
      if (pixels[at] || pixels[at+1] || pixels[at+2]) nonblack++;
    }
    return {hash,rgb,nonblack,width:w,height:h};
  }
  const onDraw = event => {
    if (!trial || event.target !== canvas) return;
    if (event.detail.revision <= trial.beforeRevision) return;
    trial.valid = true;
    trial.revision = event.detail.revision;
    const context = trial.context;
    if (!context) throw new Error('GPU draw event had no observed WebGL context');
    const firstDraw = trial.firstSubmission == null;
    submitted(context);
    if (trial.correctness && firstDraw) trial.pixels = snapshot(context);
    if (firstDraw && config.gpuFence && !trial.correctness && context.fenceSync) {
      const current = trial;
      const fence = current.fence = context.fenceSync(context.SYNC_GPU_COMMANDS_COMPLETE,0);
      current.gpuCompleted = null;
      context.flush();
      const poll = () => {
        if (current.fence !== fence) return;
        const result = context.clientWaitSync(fence,0,0);
        if (result === context.ALREADY_SIGNALED || result === context.CONDITION_SATISFIED)
          current.gpuCompleted = performance.now();
        else if (result === context.WAIT_FAILED) current.fenceError = 'GPU fence failed';
        else if (performance.now()-current.started < config.timeoutMs) setTimeout(poll,1);
      };
      setTimeout(poll,0);
    }
  };
  canvas.addEventListener('aperture-color-draw',onDraw);
  originals.push(() => canvas.removeEventListener('aperture-color-draw',onDraw));
  async function change(value, correctness = false, overlapping = false) {
    const started = performance.now();
    const current = {value,started,beforeRevision:Number(canvas.dataset.colorRevision||0),
                     valid:false,correctness,fetchBefore:fetched,
                     focused:document.hasFocus(),
                     coloredCanvases:new WeakSet(),
                     readbacksBefore:readbacks,uploadsBefore:uploads.length};
    trial = current;
    input().value = String(value);
    input().dispatchEvent(new Event('input',{bubbles:true}));
    current.dispatchEnded = performance.now();
    while (performance.now()-started < config.timeoutMs) {
      await new Promise(requestAnimationFrame);
      if (current.nextFrame) break;
      if (overlapping && trial !== current && !current.firstSubmission) {
        return {value,revision:null,document_focused:current.focused,superseded:true,
          dispatch_ms:current.dispatchEnded-started,
          first_color_composition_ms:null,first_viewport_submission_ms:null,
          last_viewport_submission_ms:null,next_frame_proxy_ms:null,
          gpu_completion_observed_ms:null,fetches:0,readbacks_2d:0,texture_uploads:[],
          started_performance_ms:started,ended_performance_ms:performance.now(),pixels:null};
      }
    }
    if (!current.lastSubmission || !current.nextFrame)
      throw new Error('Input produced no confirmed new color frame: '+value);
    if (config.gpuFence && current.fence) {
      while (!current.gpuCompleted && !current.fenceError &&
             performance.now()-started < config.timeoutMs) await sleep(1);
      current.context.deleteSync(current.fence);
      current.fence = null;
      if (current.fenceError) throw new Error(current.fenceError);
      if (!current.gpuCompleted) throw new Error('GPU fence timed out');
    }
    if (correctness && !current.pixels) current.pixels = snapshot(current.context);
    if (trial === current) trial = null;
    return {
      value, revision:current.revision ?? null,
      superseded:false,
      document_focused:current.focused,
      dispatch_ms:current.dispatchEnded-started,
      first_color_composition_ms:current.firstComposition ? current.firstComposition-started : null,
      first_viewport_submission_ms:current.firstSubmission-started,
      last_viewport_submission_ms:current.lastSubmission-started,
      next_frame_proxy_ms:current.nextFrame-started,
      gpu_completion_observed_ms:current.gpuCompleted ? current.gpuCompleted-started : null,
      fetches:fetched-current.fetchBefore,
      readbacks_2d:readbacks-current.readbacksBefore,
      texture_uploads:uploads.slice(current.uploadsBefore),
      started_performance_ms:started,ended_performance_ms:performance.now(),
      pixels:current.pixels ?? null,
    };
  }
  async function captureDisplayed() {
    const gl = [...glContexts].find(context => context.canvas === canvas);
    if (!gl) return snapshot(canvas.getContext('2d'));
    if (!viewer) throw new Error('Sustained GPU validation requires ?colorBenchmark=1');
    return await new Promise((resolve,reject) => {
      const timer = setTimeout(() => {
        canvas.removeEventListener('aperture-color-draw',capture);
        reject(new Error('Latest-setting redraw timed out'));
      },config.timeoutMs);
      function capture() {
        clearTimeout(timer);
        canvas.removeEventListener('aperture-color-draw',capture);
        try { resolve(snapshot(gl)); } catch (error) { reject(error); }
      }
      canvas.addEventListener('aperture-color-draw',capture);
      viewer.forceRedraw();
    });
  }
  try {
    const warmup = [];
    for (let i=0;i<config.warmup;i++) {
      warmup.push(await change(config.values[i%config.values.length]));
      await sleep(config.intervalMs);
    }
    frameIntervals.length = 0; previousFrame = null;
    const phaseStart = performance.now(), phaseFetches = fetched;
    const phaseReadbacks = readbacks, phaseUploads = uploads.length;
    const records = [];
    if (config.sustained) {
      const pending = [];
      let nextInput = performance.now();
      for (let i=0;i<config.trials;i++) {
        await sleep(Math.max(0,nextInput-performance.now()));
        pending.push(change(config.values[(i+config.warmup)%config.values.length],false,true));
        nextInput += config.dragIntervalMs;
        // A blocked event loop cannot deliver60Hz. Skip overdue slots.
        if (nextInput < performance.now()) nextInput = performance.now()+config.dragIntervalMs;
      }
      records.push(...await Promise.all(pending));
    } else {
      for (let i=0;i<config.trials;i++) {
        // Each trial yields to the event loop; this is not a tight input burst.
        await sleep(config.intervalMs);
        records.push(await change(config.values[(i+config.warmup)%config.values.length]));
      }
    }
    const phaseEnd = performance.now();
    const measuredCounts = {fetches:fetched-phaseFetches,
      readbacks_2d:readbacks-phaseReadbacks,texture_uploads:uploads.slice(phaseUploads)};
    const measuredFrames = [...frameIntervals];
    const latestDisplayed = config.sustained ? await captureDisplayed() : null;
    const correctness = [];
    for (const value of config.values.slice(0,2))
      correctness.push(await change(value,true));
    if (correctness[0].pixels.hash === correctness[1].pixels.hash)
      throw new Error('Slider values did not change displayed framebuffer pixels');
    let latestSettingValidation = null;
    if (latestDisplayed) {
      const expected = correctness.find(record => record.value === records.at(-1).value).pixels;
      latestSettingValidation = {value:records.at(-1).value,
        actual:latestDisplayed,expected,passed:latestDisplayed.hash===expected.hash};
      if (!latestSettingValidation.passed)
        throw new Error('Sustained edits did not display the final slider setting');
    }
    await sleep(20); // Let the observer deliver completed resource entries.
    for (const record of records) record.network_resources = resources.filter(entry =>
      entry.startTime >= record.started_performance_ms &&
      entry.startTime <= record.ended_performance_ms).map(entry =>
      ({name:entry.name,initiator:entry.initiatorType,duration_ms:entry.duration}));
    measuredCounts.network_resources = resources.filter(entry =>
      entry.startTime >= phaseStart && entry.startTime <= phaseEnd).map(entry =>
      ({name:entry.name,initiator:entry.initiatorType,duration_ms:entry.duration}));
    for (const gl of glContexts) {
      gpuInfo.push({version:gl.getParameter(gl.VERSION),
        renderer:gl.getParameter(gl.RENDERER),vendor:gl.getParameter(gl.VENDOR),
        context_lost:gl.isContextLost(),fence_available:typeof gl.fenceSync==='function'});
    }
    done({mode:canvas.dataset.colorRenderer || (glContexts.size?'webgl':'cpu'),
      filename:document.querySelector('#filename')?.textContent,
      status:document.querySelector('#status')?.textContent,
      canvas:[canvas.width,canvas.height],viewport:[innerWidth,innerHeight],
      device_pixel_ratio:devicePixelRatio,visibility:document.visibilityState,
      document_focused_at_start:focusedAtStart,document_focused_at_end:document.hasFocus(),
      gpu_info:gpuInfo, warmup, records,correctness,
      measured_counts:measuredCounts,
      geometry_at_start:geometryAtStart,geometry_at_end:geometry(),
      latest_setting_validation:latestSettingValidation,
      frame_intervals_ms:measuredFrames,
      color_compositions:composed,fetches:fetched,
      renderer_identity_note:'WebKit may sanitize vendor/renderer strings; they do not identify physical hardware.'});
  } finally {
    trial = null; running = false;
    for (const restore of originals.reverse()) restore();
    input().value = originalValue;
    input().dispatchEvent(new Event('input',{bubbles:true}));
  }
})().catch(error => done({error:String(error),stack:error.stack}));
"""


def summary(values):
    values = sorted(value for value in values if value is not None)
    if not values:
        return None
    return {
        "count": len(values),
        "p50_ms": round(values[math.ceil(len(values) * 0.50) - 1], 3),
        "p95_ms": round(values[math.ceil(len(values) * 0.95) - 1], 3),
        "max_ms": round(values[-1], 3),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--session", type=Path, required=True)
    parser.add_argument("--driver-url", help="defaults to the session driver; must match it")
    parser.add_argument(
        "--output", type=Path, default=Path("artifacts/color-benchmark/color-benchmark.json")
    )
    parser.add_argument("--selector", default='[data-color-field="maximum"]')
    parser.add_argument("--values", type=float, nargs=2, default=[64, 220])
    parser.add_argument("--trials", type=int, default=40)
    parser.add_argument("--warmup", type=int, default=4)
    parser.add_argument("--interval-ms", type=float, default=40)
    parser.add_argument("--timeout-ms", type=float, default=10000)
    parser.add_argument("--gpu-fence", action="store_true")
    parser.add_argument("--sustained", action="store_true")
    parser.add_argument("--drag-interval-ms", type=float, default=1000 / 60)
    args = parser.parse_args()
    args.driver_url = compositor.driver_url(args.driver_url)
    if args.trials < 2 or args.warmup < 2 or args.interval_ms < 1:
        parser.error("Use at least two trials/warmups and a positive input interval")
    if not all(
        math.isfinite(value)
        for value in [*args.values, args.interval_ms, args.drag_interval_ms, args.timeout_ms]
    ) or args.drag_interval_ms < 1 or args.timeout_ms <= 0:
        parser.error("Values and timing options must be finite with positive intervals/timeouts")
    session = json.loads(args.session.read_text())
    sid = session.get("session_id") or session.get("sessionId")
    if not sid:
        parser.error("Session JSON must contain session_id or sessionId")
    config = {
        "selector": args.selector,
        "values": args.values,
        "trials": args.trials,
        "warmup": args.warmup,
        "intervalMs": args.interval_ms,
        "timeoutMs": args.timeout_ms,
        "gpuFence": args.gpu_fence,
        "sustained": args.sustained,
        "dragIntervalMs": args.drag_interval_ms,
    }
    script_timeout = (args.trials + args.warmup + 3) * (
        args.timeout_ms + args.interval_ms
    ) + 30000

    def post(endpoint, body):
        request = Request(
            args.driver_url + f"/session/{sid}/" + endpoint,
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urlopen(request, timeout=script_timeout / 1000 + 10) as response:
                value = json.load(response)["value"]
        except HTTPError as error:
            raise RuntimeError(error.read().decode()) from error
        if isinstance(value, dict) and "error" in value:
            raise RuntimeError(value)
        return value

    post("timeouts", {"script": int(script_timeout)})
    result = post("execute/async", {"script": SCRIPT, "args": [config]})
    if "error" in result:
        raise RuntimeError(result)
    result["config"] = config
    result["timing_note"] = (
        "CPU submission is not GPU completion. Next-frame proxy is the next "
        "animation callback after the first confirmed color draw, not physical "
        "screen presentation. GPU completion is fence observation, including "
        "polling delay. Pixel readbacks occur only in the separate correctness pass."
    )
    result["sustained_note"] = (
        "Timers deliver inputs at the requested cadence only while the event loop "
        "can keep up. GPU revisions identify submitted settings; CPU uses fresh "
        "color-canvas provenance. Superseded inputs have no qualifying draw. "
        "A following animation callback is a proxy even when later input replaces "
        "a previously submitted frame before the screen presents it."
    ) if args.sustained else None
    result["summary"] = {
        field: summary(record[field] for record in result["records"])
        for field in [
            "dispatch_ms",
            "first_color_composition_ms",
            "first_viewport_submission_ms",
            "last_viewport_submission_ms",
            "next_frame_proxy_ms",
            "gpu_completion_observed_ms",
        ]
    }
    result["summary"]["frame_interval"] = summary(result["frame_intervals_ms"])
    result["summary"]["input_interval"] = summary(
        current["started_performance_ms"] - previous["started_performance_ms"]
        for previous, current in zip(result["records"], result["records"][1:])
    )
    counts = result["measured_counts"]
    result["summary"]["measured_fetches"] = counts["fetches"]
    result["summary"]["focused_trials"] = sum(
        r["document_focused"] for r in result["records"]
    )
    result["summary"]["submitted_inputs"] = sum(
        r["first_viewport_submission_ms"] is not None for r in result["records"]
    )
    result["summary"]["superseded_inputs"] = sum(
        r["superseded"] for r in result["records"]
    )
    result["summary"]["measured_network_resources"] = len(counts["network_resources"])
    result["summary"]["measured_2d_readbacks"] = counts["readbacks_2d"]
    upload_counts = {}
    for upload in counts["texture_uploads"]:
        key = json.dumps(upload, sort_keys=True)
        upload_counts[key] = upload_counts.get(key, 0) + 1
    result["summary"]["measured_texture_uploads"] = [
        {**json.loads(key), "count": count} for key, count in upload_counts.items()
    ]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(
        json.dumps(
            {
                "mode": result["mode"],
                "canvas": result["canvas"],
                "summary": result["summary"],
                "output": str(args.output),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
