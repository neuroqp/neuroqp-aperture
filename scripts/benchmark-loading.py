#!/usr/bin/env python3
"""Measure first image draw in the native Tauri app, using its real renderer/IPC.

Run inside scripts/headless-session --driver 4448 --keep DIR, with DIR inside
--output's directory. The chooser response alone is supplied by the harness;
Open still uses the real frontend and backend. No source pixels or annotations are changed. Cache-cold
means no completed app cache, not a cold operating-system filesystem cache.
"""

import argparse
import base64
import json
import os
from http.client import RemoteDisconnected
from pathlib import Path
import re
import shutil
import statistics
import threading
import time
from urllib.request import Request, urlopen

import compositor


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("images", type=Path, nargs="+")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache-root", type=Path, required=True)
    parser.add_argument("--driver-url", help="defaults to the session driver; must match it")
    parser.add_argument("--driver-pid", type=int, help="Linux process-tree RSS sampling through preview and pyramid completion")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--warm-only", action="store_true", help="Reuse existing app caches without clearing them")
    parser.add_argument("--fresh-only", action="store_true", help="Measure new-file loading without a cached reopen")
    parser.add_argument("--evict-source-pages", action="store_true", help="Ask Linux to discard this source's clean page cache before each fresh trial")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--cancel-after-preview", type=Path, action="append", default=[])
    args = parser.parse_args()
    args.driver_url = compositor.driver_url(args.driver_url)
    if args.warm_only and args.fresh_only:
        parser.error("--warm-only and --fresh-only are mutually exclusive")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Only our dedicated benchmark cache can be cleared.
    cache = args.cache_root.resolve()
    output_dir = args.output.parent.resolve()
    if cache == output_dir or not cache.is_relative_to(output_dir) or cache.name != "display-v2":
        parser.error("--cache-root must be a dedicated display-v2 descendant of the output directory")
    if any(path.resolve().is_relative_to(cache) for path in args.images):
        parser.error("Source images must be outside the benchmark cache")
    cancel_paths = {path.resolve() for path in args.cancel_after_preview}

    def request(method, route, body=None):
        req = Request(
            args.driver_url + route,
            data=None if body is None else json.dumps(body).encode(),
            method=method,
            headers={"Content-Type": "application/json"},
        )
        # WebKitWebDriver can close a connection during a page transition.
        # Retry inspection and idempotent cancellation only. Never replay Open
        # or other UI mutations when their first response is lost.
        retry_safe = method == "GET" or (route.endswith("/execute/sync") and body["script"].startswith("return ")) or (
            route.endswith("/execute/async") and body.get("args", [None])[0] in ("image_state", "cancel_load")
        )
        for attempt in range(3):
            try:
                with urlopen(req, timeout=30) as response:
                    value = json.load(response).get("value")
                break
            except RemoteDisconnected:
                if not retry_safe or attempt == 2:
                    raise
                time.sleep(0.05)
        if isinstance(value, dict) and isinstance(value.get("error"), str):
            raise RuntimeError(value)
        return value

    sid = request("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "wry",
        "tauri:options": {"application": str(compositor.application(args.binary))},
    }}})["sessionId"]
    args.output.with_suffix(".session.json").write_text(json.dumps({"sessionId": sid}) + "\n")
    route = f"/session/{sid}"

    def js(script, values=None, asynchronous=False):
        return request("POST", route + "/execute/" + ("async" if asynchronous else "sync"), {
            "script": script, "args": values or [],
        })

    def invoke(command):
        return js("const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke(arguments[0]).then(done,e=>done({error:String(e)}));", [command], True)

    records = []
    def rss_monitor(stop, peaks):
        while not stop.is_set():
            pending = [args.driver_pid]
            seen = set()
            tree = decoder = 0
            while pending:
                pid = pending.pop()
                if pid in seen:
                    continue
                seen.add(pid)
                proc = Path(f"/proc/{pid}")
                try:
                    status = proc.joinpath("status").read_text()
                    match = re.search(r"^VmRSS:\s+(\d+)", status, re.M)
                    rss = int(match[1]) if match else 0
                    tree += rss
                    if b"--image-worker" in proc.joinpath("cmdline").read_bytes():
                        decoder += rss
                    for task in proc.joinpath("task").iterdir():
                        pending.extend(map(int, task.joinpath("children").read_text().split()))
                except (OSError, ValueError):
                    continue
            peaks["treePeakRssMiB"] = max(peaks.get("treePeakRssMiB", 0), round(tree / 1024, 1))
            peaks["decoderPeakRssMiB"] = max(peaks.get("decoderPeakRssMiB", 0), round(decoder / 1024, 1))
            stop.wait(0.05)
    before = {str(path.resolve()): (path.stat().st_size, path.stat().st_mtime_ns) for path in args.images}
    try:
        url = request("GET", route + "/url").split("?")[0]
        request("POST", route + "/url", {"url": url + "?colorBenchmark=1"})
        deadline = time.monotonic() + 20
        while not js("return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;"):
            if time.monotonic() > deadline:
                raise TimeoutError("Native application did not initialize")
            time.sleep(0.02)
        renderer = js("return window.__apertureViewer.drawer.getType();")
        js("""
          const viewer=window.__apertureViewer;
          const originalInvoke=window.__TAURI_INTERNALS__.invoke;
          const originalFetch=window.fetch;
          window.__loadBenchmark={};
          window.fetch=function(url,options) {
            const metric=window.__loadBenchmark;
            if(String(url).includes('plugin%3Adialog%7Copen') && metric.path) {
              metric.selectedAt=performance.now();
              return Promise.resolve(new Response(JSON.stringify(metric.path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            }
            if(String(url).endsWith('/open_image')) metric.openAt=performance.now();
            return originalFetch.call(this,url,options);
          };
          const draw=viewer.drawer.draw;
          viewer.drawer.draw=function(items) {
            const result=draw.call(this,items), metric=window.__loadBenchmark;
            if(metric.openAt && !metric.firstDrawMs) {
              const preview=items.find(item=>{
                const levels=new Map();
                for(const {tile} of item.getTilesToDraw()) {
                  if(!(tile.opacity>0) || !this.getDataToDraw(tile)) continue;
                  const tiles=levels.get(tile.level) ?? new Set();
                  tiles.add(`${tile.x},${tile.y}`);levels.set(tile.level,tiles);
                }
                return [...levels].some(([level,tiles])=>{
                  const count=item.source.getNumTiles(level);
                  return tiles.size===count.x*count.y;
                });
              });
              const drawn=preview && (this.packets?.some(packet=>packet.opacity>0) || this.getType()==='canvas');
              if(drawn) {
                metric.firstDrawMs=performance.now()-metric.selectedAt;
                metric.firstDrawKind=preview.source.maxLevel===0 ? 'preview' : 'complete-detail-level';
                const tile=preview.getTilesToDraw()[0]?.tile;
                const data=tile && this.getDataToDraw(tile);
                metric.previewWidth=data?.width ?? data?.canvas?.width;
                metric.previewHeight=data?.height ?? data?.canvas?.height;
                if(this.gl) {
                  const pixel=new Uint8Array(4);
                  this.gl.readPixels(Math.floor(this.canvas.width/2),Math.floor(this.canvas.height/2),1,1,this.gl.RGBA,this.gl.UNSIGNED_BYTE,pixel);
                  metric.centerPixel=Array.from(pixel);
                }
                requestAnimationFrame(()=>{ if(window.__loadBenchmark===metric) metric.nextFrameMs=performance.now()-metric.selectedAt; });
              }
            }
            return result;
          };
          setInterval(()=>{
            const metric=window.__loadBenchmark;
            if(metric.openAt && !metric.backendReadyMs)
              originalInvoke('image_state').then(state=>{
                if(window.__loadBenchmark===metric && state?.previewReady && !metric.backendPreviewMs)
                  metric.backendPreviewMs=performance.now()-metric.selectedAt;
                if(window.__loadBenchmark===metric && state?.phase==='ready' && !metric.backendReadyMs)
                  metric.backendReadyMs=performance.now()-metric.selectedAt;
              });
          },10);
          return true;
        """)
        for image in args.images:
            for round_index in range(args.rounds):
                for condition in (("reopen",) if args.warm_only else ("fresh",) if args.fresh_only else ("fresh", "reopen")):
                    invoke("cancel_load")
                    time.sleep(0.05)
                    if condition == "fresh" and cache.exists():
                        for entry in cache.iterdir():
                            # Only recognized completed caches, never evidence or live work.
                            if (re.fullmatch(r"[0-9a-f]{16}", entry.name)
                                    and not entry.is_symlink() and entry.is_dir()
                                    and (entry / "complete").is_file()):
                                shutil.rmtree(entry)
                    if condition == "fresh" and args.evict_source_pages:
                        # Read-only, file-local advisory eviction. This changes
                        # neither source bytes nor the system-wide cache policy.
                        with image.open("rb") as source:
                            os.posix_fadvise(source.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)
                    stop = threading.Event()
                    peaks = {}
                    monitor = threading.Thread(target=rss_monitor, args=(stop, peaks)) if args.driver_pid else None
                    if monitor:
                        monitor.start()
                    js("window.__loadBenchmark={path:arguments[0]};document.getElementById('open-button').click();return true;", [str(image.resolve())])
                    deadline = time.monotonic() + args.timeout
                    while True:
                        metric = js("return window.__loadBenchmark;")
                        if metric.get("nextFrameMs"):
                            break
                        status = js("return {phase:document.getElementById('viewer').dataset.phase,status:document.getElementById('status').textContent};")
                        if status["phase"] == "error":
                            raise RuntimeError(status)
                        if time.monotonic() > deadline:
                            args.output.with_suffix(".timeout.png").write_bytes(base64.b64decode(request("GET", route + "/screenshot")))
                            raise TimeoutError((image, condition, metric, status))
                        time.sleep(0.01)
                    metric.update(image=image.name, condition=condition, round=round_index, renderer=renderer, sourcePageEvictionRequested=condition == "fresh" and args.evict_source_pages)
                    if round_index == 0 and condition == "fresh":
                        screenshot = args.output.parent / f"{args.output.stem}-{image.stem}.png"
                        screenshot.write_bytes(base64.b64decode(request("GET", route + "/screenshot")))
                        metric["screenshot"] = str(screenshot)
                    records.append(metric)
                    if image.resolve() in cancel_paths:
                        invoke("cancel_load")
                        stop.set()
                        if monitor:
                            monitor.join()
                            metric.update(peaks)
                        print(json.dumps(metric), flush=True)
                        break
                    while (state := invoke("image_state"))["phase"] == "loading":
                        if time.monotonic() > deadline:
                            raise TimeoutError((image, "detail", state))
                        time.sleep(0.01)
                    if state["phase"] != "ready":
                        raise RuntimeError(state)
                    if condition == "fresh" and "Reused display cache" in state["detail"]:
                        raise RuntimeError("Fresh trial reused a cache; check the driver's XDG_CACHE_HOME")
                    if args.warm_only and "Reused display cache" not in state["detail"]:
                        raise RuntimeError(f"No completed cache for {image}")
                    metric.update(js("const m=window.__loadBenchmark;if(!m.backendReadyMs)m.backendReadyMs=performance.now()-m.selectedAt;return m;"))
                    stop.set()
                    if monitor:
                        monitor.join()
                        metric.update(peaks)
                    print(json.dumps(metric), flush=True)
                    args.output.write_text(json.dumps({"records": records}, indent=2) + "\n")
        summary = []
        for image in args.images:
            for condition in ("fresh", "reopen"):
                group = [row for row in records if row["image"] == image.name and row["condition"] == condition]
                if group:
                    summary.append({"image": image.name, "condition": condition, "samples": len(group),
                        **{key: round(statistics.median(row[key] for row in group), 1) for key in ("backendPreviewMs", "firstDrawMs", "nextFrameMs", "backendReadyMs") if all(key in row for row in group)}})
        args.output.write_text(json.dumps({"records": records, "summary": summary}, indent=2) + "\n")
        print(json.dumps(summary, indent=2))
    finally:
        if "stop" in locals():
            stop.set()
            if monitor:
                monitor.join()
        request("DELETE", route)
        for path, stat in before.items():
            actual = Path(path).stat()
            assert (actual.st_size, actual.st_mtime_ns) == stat, f"Source changed: {path}"


if __name__ == "__main__":
    main()
