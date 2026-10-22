#!/usr/bin/env python3
"""Validate 16-bit display in an owned native Aperture session (stdlib WebDriver).

Build the application, then run inside scripts/headless-session --driver 4446,
which isolates cache/config directories. This script writes small deterministic
fixtures to the ignored test-data/16bit/ (16-bit gray, RGB16, ImageJ 3-channel,
3-band multiband and an 8-bit RGB control), opens each through the real Open
handler with only the chooser response substituted, and checks:

- `.u16` previews and full-resolution tiles equal the source samples exactly;
- Color starts at the sampled source min/max (shared for RGB16), 0-255 for 8-bit;
- rendered pixels follow the analytic transfer, so low-intensity data is visible;
- the GPU drawer reports webgl2, and GPU and CPU (?colorRenderer=cpu) agree;
- fixtures are unchanged. A JSON report is written under artifacts/16bit/.
"""

import argparse
import base64
import hashlib
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import time
from http.client import RemoteDisconnected
from urllib.request import Request, urlopen

import compositor

ROOT = Path(__file__).resolve().parents[1]
TILE = 256


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_tiff(path, width, height, pages, bits, photometric, samples=1, description=None):
    """Write an uncompressed little-endian TIFF; `pages` are flat sample lists."""
    kind = 'H' if bits == 16 else 'B'
    out = bytearray(b'II*\x00\x00\x00\x00\x00')

    def align():
        if len(out) % 2:
            out.append(0)

    offsets = []
    for page in pages:
        align()
        offsets.append(len(out))
        out += struct.pack(f'<{len(page)}{kind}', *page)
    previous = 4
    for index, data_offset in enumerate(offsets):
        extra = {}
        align()
        if samples > 1:
            extra['bits'] = len(out)
            out += struct.pack(f'<{samples}H', *([bits] * samples))
        if description and index == 0:
            align()
            extra['description'] = len(out)
            out += description.encode() + b'\x00'
        entries = [
            (256, 4, 1, width), (257, 4, 1, height),
            (258, 3, samples, extra['bits'] if samples > 1 else bits),
            (259, 3, 1, 1), (262, 3, 1, photometric),
        ]
        if 'description' in extra:
            entries.append((270, 2, len(description) + 1, extra['description']))
        entries += [
            (273, 4, 1, data_offset), (277, 3, 1, samples), (278, 4, 1, height),
            (279, 4, 1, width * height * samples * bits // 8), (284, 3, 1, 1),
        ]
        if samples > 1 and photometric == 1:
            entries.append((338, 3, samples - 1, 0))  # unspecified extra samples
        align()
        out[previous:previous + 4] = struct.pack('<I', len(out))
        out += struct.pack('<H', len(entries))
        for tag, kind_id, count, value in entries:
            inline = struct.pack('<H', value) + b'\x00\x00' if kind_id == 3 and count == 1 else struct.pack('<I', value)
            out += struct.pack('<HHI', tag, kind_id, count) + inline
        previous = len(out)
        out += b'\x00\x00\x00\x00'
    path.write_bytes(bytes(out))


def pattern(width, height, low, high, a, b):
    """Deterministic samples covering exactly [low, high]."""
    span = high - low + 1
    values = [low + (x * a + y * b) % span for y in range(height) for x in range(width)]
    values[0], values[-1] = low, high
    return values


def fixtures(directory):
    directory.mkdir(parents=True, exist_ok=True)
    specs = []
    w, h = 257, 131
    gray = pattern(w, h, 100, 4000, 37, 101)
    specs.append({'name': 'gray16-low-257x131.tif', 'width': w, 'height': h, 'bits': 16,
                  'planes': [gray], 'rgb': False, 'interpretation': 'grey16',
                  'write': lambda p: write_tiff(p, w, h, [gray], 16, 1)})
    rw, rh = 193, 97
    rgb = [pattern(rw, rh, 200, 3000, 13, 7), pattern(rw, rh, 500, 5000, 29, 3),
           pattern(rw, rh, 50, 1500, 5, 17)]
    interleaved = [rgb[c][i] for i in range(rw * rh) for c in range(3)]
    specs.append({'name': 'rgb16-193x97.tif', 'width': rw, 'height': rh, 'bits': 16,
                  'planes': rgb, 'rgb': True, 'interpretation': 'rgb16',
                  'write': lambda p: write_tiff(p, rw, rh, [interleaved], 16, 2, 3)})
    iw, ih = 131, 67
    imagej = [pattern(iw, ih, 300, 1200, 3, 11), pattern(iw, ih, 1000, 20000, 211, 97),
              pattern(iw, ih, 40000, 65535, 503, 7)]
    description = 'ImageJ=1.54f\nimages=3\nchannels=3\nhyperstack=true\nmode=composite\n'
    specs.append({'name': 'imagej-3c16-131x67.tif', 'width': iw, 'height': ih, 'bits': 16,
                  'planes': imagej, 'rgb': False, 'interpretation': 'grey16',
                  'write': lambda p: write_tiff(p, iw, ih, imagej, 16, 1, description=description)})
    mw, mh = 101, 53
    multi = [pattern(mw, mh, 10, 900, 7, 3), pattern(mw, mh, 2000, 2600, 11, 5),
             pattern(mw, mh, 0, 65535, 4099, 257)]
    multi_interleaved = [multi[c][i] for i in range(mw * mh) for c in range(3)]
    specs.append({'name': 'multiband3-16-101x53.tif', 'width': mw, 'height': mh, 'bits': 16,
                  'planes': multi, 'rgb': False, 'interpretation': None,
                  'write': lambda p: write_tiff(p, mw, mh, [multi_interleaved], 16, 1, 3)})
    cw, ch = 129, 65
    control = [pattern(cw, ch, 0, 255, 3, 5), pattern(cw, ch, 20, 200, 7, 1),
               pattern(cw, ch, 64, 128, 1, 9)]
    control_interleaved = [control[c][i] for i in range(cw * ch) for c in range(3)]
    specs.append({'name': 'rgb8-control-129x65.tif', 'width': cw, 'height': ch, 'bits': 8,
                  'planes': control, 'rgb': True, 'interpretation': 'srgb',
                  'write': lambda p: write_tiff(p, cw, ch, [control_interleaved], 8, 2, 3)})
    for spec in specs:
        spec['path'] = (directory / spec['name']).resolve()
        spec['write'](spec['path'])
        del spec['write']
        header = subprocess.run(['vipsheader', '-a', str(spec['path'])], capture_output=True, text=True, check=True).stdout
        fields = dict(line.split(': ', 1) for line in header.splitlines() if ': ' in line)
        expected_format = 'ushort' if spec['bits'] == 16 else 'uchar'
        assert fields.get('format') == expected_format and int(fields['width']) == spec['width'], (spec['name'], fields)
        if spec['interpretation']:
            assert fields.get('interpretation') == spec['interpretation'], (spec['name'], fields)
        spec['vips'] = {k: fields.get(k) for k in ('format', 'interpretation', 'bands', 'n-pages')}
        spec['ranges'] = [[min(p), max(p)] for p in spec['planes']]
        if spec['rgb'] and spec['bits'] == 16:
            shared = [min(r[0] for r in spec['ranges']), max(r[1] for r in spec['ranges'])]
            spec['initial'] = [shared] * 3
        elif spec['bits'] == 16:
            spec['initial'] = spec['ranges']
        else:
            spec['initial'] = [[0, 255]] * len(spec['planes'])
    return specs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', help='defaults to the session driver; must match it')
    parser.add_argument('--fixtures', type=Path, default=ROOT / 'test-data/16bit')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/16bit/native-16bit.json')
    parser.add_argument('--timeout', type=float, default=90)
    parser.add_argument('--renderer', choices=('gpu', 'cpu'), default='gpu',
                        help='Renderer of the first pass; the other renderer runs second for comparison.')
    args = parser.parse_args()
    args.driver_url = compositor.driver_url(args.driver_url)
    desktop = compositor.session()
    args.output = args.output.absolute()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    binary = args.binary.resolve()
    specs = fixtures(args.fixtures.resolve())
    hashes = {str(spec['path']): digest(spec['path']) for spec in specs}
    passes = [args.renderer, 'cpu' if args.renderer == 'gpu' else 'gpu']
    report = {'passed': False, 'renderers': passes, 'checks': {}, 'sourceHashes': hashes,
              'fixtures': [{k: spec[k] for k in ('name', 'width', 'height', 'bits', 'rgb', 'ranges', 'initial', 'vips')} for spec in specs],
              'screenshots': []}

    def app_processes():
        found = set()
        for entry in Path('/proc').iterdir():
            if entry.name.isdigit():
                try:
                    if Path(os.readlink(entry / 'exe')) == binary:
                        found.add(int(entry.name))
                except (FileNotFoundError, PermissionError, ProcessLookupError):
                    pass
        return found

    previous_processes = app_processes()

    def request(method, path, body=None, retry=True):
        req = Request(args.driver_url + path, method=method,
                      data=None if body is None else json.dumps(body).encode(),
                      headers={'Content-Type': 'application/json'})
        try:
            with urlopen(req, timeout=max(30, args.timeout)) as response:
                value = json.load(response).get('value')
        except (ConnectionError, RemoteDisconnected) as error:
            # WebKitWebDriver occasionally resets a connection; retry once, visibly.
            if not retry:
                raise
            report.setdefault('driverRetries', []).append({'method': method, 'path': path, 'error': repr(error)})
            time.sleep(1)
            return request(method, path, body, False)
        if isinstance(value, dict) and value.get('error') and 'message' in value:
            raise RuntimeError(value)
        return value

    sid = request('POST', '/session', retry=False, body={'capabilities': {'alwaysMatch': {
        'browserName': 'wry', 'tauri:options': {'application': str(compositor.application(binary))},
    }}})['sessionId']
    route = f'/session/{sid}'
    args.output.with_suffix('.session.json').write_text(json.dumps({'sessionId': sid}) + '\n')

    def js(script, values=None, asynchronous=False):
        return request('POST', route + '/execute/' + ('async' if asynchronous else 'sync'),
                       {'script': script, 'args': values or []})

    def invoke(command, values=None):
        return js('const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(done,e=>done({error:String(e)}));', [command, values or {}], True)

    def state():
        return invoke('image_state')

    def ui():
        return js("return {phase:document.getElementById('viewer').dataset.phase,name:document.getElementById('filename').textContent,status:document.getElementById('status').textContent};")

    def wait(predicate, description, timeout=None):
        deadline = time.monotonic() + (timeout or args.timeout)
        while time.monotonic() < deadline:
            if result := predicate():
                return result
            time.sleep(.05)
        raise TimeoutError(f'{description}: {state()} {ui()}')

    def screenshot(name):
        path = args.output.parent / (name + '.png')
        path.write_bytes(base64.b64decode(request('GET', route + '/screenshot')))
        report['screenshots'].append(str(path))
        if os.environ.get('APERTURE_SESSION') == 'headless':
            # The composited nested output, as a user would see the window.
            path = args.output.parent / (name + '.session.png')
            desktop.screenshot(path)
            report['screenshots'].append(str(path))

    def pin_window():
        # Tiling sizes clients to the output. Float only this harness-owned window
        # at a fixed size so both passes compare frames of the same geometry.
        owned = app_processes() - previous_processes
        windows = [w for w in desktop.windows() if w['pid'] in owned and w['app_id'] == compositor.APP_ID]
        assert len(windows) == 1, f'Expected exactly one owned validation window: {windows}'
        desktop.resize(windows[0], 1280, 900)
        request('POST', route + '/window/rect', {'width': 1280, 'height': 900})
        size = lambda: js('return [innerWidth,innerHeight];')
        wait(lambda: (first := size()) and (time.sleep(.5) or True) and first == size() and first, 'stable window size')

    def start(renderer):
        url = request('GET', route + '/url').split('?')[0]
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1' + ('&colorRenderer=cpu' if renderer == 'cpu' else '')})
        wait(lambda: js('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;'), 'app startup')
        pin_window()
        js("""
          window.__16bitTest={path:null};const original=window.fetch;
          window.fetch=function(url,options){
            if(String(url).includes('plugin%3Adialog%7Copen'))
              return Promise.resolve(new Response(JSON.stringify(window.__16bitTest.path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            return original.call(this,url,options);
          };
        """)

    def open_ready(path):
        previous = state()
        previous_id = previous['id'] if previous else None
        js("window.__16bitTest.path=arguments[0];document.getElementById('open-button').click();", [str(path)])

        def current():
            s = state()
            if s and s.get('path') == str(path) and s['id'] != previous_id and s.get('phase') == 'error':
                raise RuntimeError(s.get('error') or s)
            return s if s and s.get('path') == str(path) and s.get('phase') == 'ready' and s['id'] != previous_id else False
        result = wait(current, 'ready ' + path.name)
        wait(lambda: ui()['phase'] == 'ready' and ui()['name'] == path.name, 'rendered ready')
        # Every visible item belongs to this id and has drawn all of its tiles.
        try:
            wait(lambda: js("""const v=window.__apertureViewer,n=v.world.getItemCount();
              return n>0 && Array.from({length:n},(_,i)=>v.world.getItemAt(i)).every(item=>item.source.getTileUrl(item.source.maxLevel,0,0).includes('/'+arguments[0]+'/') && item.getFullyLoaded());""",
                            [result['id']]), 'fully loaded tiles for ' + path.name)
        except TimeoutError as error:
            raise TimeoutError(str(error), js("""const v=window.__apertureViewer;
              return {visibility:document.visibilityState,focus:document.hasFocus(),loader:{jobs:v.imageLoader.jobsInProgress,queue:v.imageLoader.jobQueue?.length},
                items:Array.from({length:v.world.getItemCount()},(_,i)=>{const item=v.world.getItemAt(i);
                  return {url:item.source.getTileUrl(item.source.maxLevel,0,0),opacity:item.getOpacity(),fully:item.getFullyLoaded(),
                    tiles:item.getTilesToDraw().map(t=>({level:t.tile.level,x:t.tile.x,y:t.tile.y,loaded:t.tile.loaded,loading:t.tile.loading,exists:t.tile.exists}))};})};""")) from error
        return result

    def fetch_samples(s, paths):
        """Fetch asset paths below the current image id; return status, type and bytes."""
        return js("""
          const done=arguments[arguments.length-1],s=arguments[0],paths=arguments[1];
          const source=window.__apertureViewer.world.getItemAt(0).source;
          const url=source.getTileUrl(source.maxLevel,0,0);
          if(!url.includes('/'+s.id+'/')){done({error:'Displayed source id differs from backend'});return;}
          const base=url.split('/'+s.id+'/')[0]+'/'+s.id;
          const encode=bytes=>{let text='';for(let i=0;i<bytes.length;i+=32768)text+=String.fromCharCode.apply(null,bytes.subarray(i,i+32768));return btoa(text);};
          Promise.all(paths.map(path=>fetch(base+path).then(async r=>({status:r.status,type:r.headers.get('content-type'),
            data:r.ok?encode(new Uint8Array(await r.arrayBuffer())):''})))).then(done,e=>done({error:String(e)}));
        """, [s, paths], True)

    def u16(entry):
        body = base64.b64decode(entry['data'])
        width, height = struct.unpack('<II', body[:8])
        assert len(body) == 8 + 2 * width * height, (len(body), width, height)
        return width, height, list(struct.unpack(f'<{width * height}H', body[8:]))

    def check_assets(s, spec):
        channels = len(spec['planes'])
        w, h = spec['width'], spec['height']
        if spec['bits'] == 8:
            missing = fetch_samples(s, ['/channels/0/preview.u16'])
            assert isinstance(missing, list) and missing[0]['status'] == 404, missing
            return {'u16RejectedFor8Bit': missing[0]['status']}
        level = (max(w, h) - 1).bit_length()
        columns, rows = -(-w // TILE), -(-h // TILE)
        paths = [f'/channels/{c}/preview.u16' for c in range(channels)]
        paths += [f'/channels/{c}/tiles/{level}/{x}_{y}.u16' for c in range(channels)
                  for y in range(rows) for x in range(columns)]
        result = fetch_samples(s, paths)
        assert isinstance(result, list), result
        assert all(r['status'] == 200 and r['type'] == 'application/octet-stream' for r in result), \
            [(p, r['status'], r['type']) for p, r in zip(paths, result)]
        for c in range(channels):
            pw, ph, preview = u16(result[c])
            assert (pw, ph) == (w, h) and preview == spec['planes'][c], (spec['name'], c, pw, ph)
        tiles = iter(result[channels:])
        for c in range(channels):
            stitched = [None] * (w * h)
            for y in range(rows):
                for x in range(columns):
                    tw, th, tile = u16(next(tiles))
                    assert (tw, th) == (min(TILE, w - x * TILE), min(TILE, h - y * TILE)), (spec['name'], c, x, y, tw, th)
                    for row in range(th):
                        at = (y * TILE + row) * w + x * TILE
                        stitched[at:at + tw] = tile[row * tw:(row + 1) * tw]
            assert stitched == spec['planes'][c], (spec['name'], c, 'full-resolution tiles differ')
        return {'previewsExact': channels, 'tilesExact': channels * rows * columns, 'level': level}

    def color_ranges():
        """Select each connection in the real Color panel and read its range outputs."""
        return js("""
          if(document.getElementById('color-panel').hidden)document.getElementById('color-button').click();
          const ids=[...new Set([...document.querySelectorAll('#color-wires [data-edge]')].map(e=>e.getAttribute('data-edge')))].sort((a,b)=>a-b);
          return ids.map(id=>{
            document.querySelector('[data-edge="'+id+'"]').dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
            const value=f=>document.getElementById('color-value-'+f)?.textContent;
            return {id,label:document.getElementById('color-range-minimum')?.getAttribute('aria-label'),
              minimum:Number(value('minimum')),maximum:Number(value('maximum')),brightness:value('brightness')};
          });
        """)

    def readback(points, full=False):
        result = js("""
          const done=arguments[arguments.length-1],points=arguments[0],full=arguments[1],v=window.__apertureViewer;
          const canvas=v.drawer.canvas,gl=canvas.dataset.colorRenderer==='webgl2'?canvas.getContext('webgl2'):null;
          const encode=bytes=>{let text='';for(let i=0;i<bytes.length;i+=32768)text+=String.fromCharCode.apply(null,bytes.subarray(i,i+32768));return btoa(text);};
          function capture(){try{
            const w=canvas.width,h=canvas.height;let px;
            if(gl){const raw=new Uint8Array(w*h*4);gl.readPixels(0,0,w,h,gl.RGBA,gl.UNSIGNED_BYTE,raw);
              if(gl.getError()!==gl.NO_ERROR)throw new Error('Framebuffer read failed');
              px=new Uint8Array(w*h*4);for(let y=0;y<h;y++)px.set(raw.subarray((h-1-y)*w*4,(h-y)*w*4),y*w*4);}
            else px=canvas.getContext('2d').getImageData(0,0,w,h).data;
            // OSD's Point prototype lacks a constructor property; build points from it.
            const item=v.world.getItemAt(v.world.getItemCount()-1),proto=Object.getPrototypeOf(v.viewport.getCenter()),ratio=w/canvas.clientWidth;
            const samples=points.map(([x,y])=>{const e=item.imageToViewerElementCoordinates(Object.assign(Object.create(proto),{x:x+.5,y:y+.5}));
              const cx=Math.floor(e.x*ratio),cy=Math.floor(e.y*ratio);if(cx<0||cy<0||cx>=w||cy>=h)return null;
              const at=(cy*w+cx)*4;return [px[at],px[at+1],px[at+2]];});
            let hash=2166136261,nonblack=0,maximum=0;
            for(let i=0;i<px.length;i+=4){for(let c=0;c<3;c++){hash=Math.imul(hash^px[i+c],16777619)>>>0;maximum=Math.max(maximum,px[i+c]);}if(px[i]||px[i+1]||px[i+2])nonblack++;}
            done({width:w,height:h,ratio,hash,nonblack,maximum,samples,colorRenderer:canvas.dataset.colorRenderer,drawer:v.drawer.getType(),
              pixels:full?encode(px):undefined});
          }catch(e){done({error:String(e)});}}
          if(gl){const timer=setTimeout(()=>{canvas.removeEventListener('aperture-color-draw',drawn);done({error:'Viewport redraw timed out'});},10000);
            function drawn(){clearTimeout(timer);canvas.removeEventListener('aperture-color-draw',drawn);capture();}
            canvas.addEventListener('aperture-color-draw',drawn);v.forceRedraw();}
          else{v.forceRedraw();requestAnimationFrame(()=>requestAnimationFrame(capture));}
        """, [points, full], True)
        assert not result.get('error'), result
        return result

    def expected_pixel(spec, colors, ranges, index):
        total = [0.0, 0.0, 0.0]
        for c, plane in enumerate(spec['planes']):
            low, high = ranges[c]
            intensity = max(0.0, min(1.0, (plane[index] - low) / (high - low)))
            for k in range(3):
                total[k] += intensity * colors[c][k]
        return [max(0, min(255, round(value))) for value in total]

    def run_pass(renderer):
        start(renderer)
        results = {}
        for spec in specs:
            name = spec['name']
            s = open_ready(spec['path'])
            channels = len(spec['planes'])
            sixteen = spec['bits'] == 16
            assert len(s['channels']) == channels and s['sampleBits'] == spec['bits'], s
            if sixteen:
                assert bool(s['rgb']) == spec['rgb'], s
            assert bool(s.get('channelMode')) == sixteen, s
            assert all(c['domain'] == ([0, 65535] if sixteen else [0, 255]) for c in s['channels']), s
            # displayRanges are ND2/CZI frame metadata; TIFFs leave them empty.
            assert s.get('displayRanges') in ([], None, spec['ranges']), s.get('displayRanges')
            entry = {'state': {k: s.get(k) for k in ('width', 'height', 'sampleBits', 'rgb', 'channelMode', 'displayRanges', 'channels')}}
            entry['assets'] = check_assets(s, spec)
            # The initial range is set once from the first preview planes.
            try:
                ranges = wait(lambda: (r := color_ranges()) and len(r) == channels and
                              [[e['minimum'], e['maximum']] for e in r] == spec['initial'] and r,
                              'initial Color ranges ' + name, timeout=20)
            except TimeoutError:
                raise AssertionError(('initial Color ranges', name, color_ranges(), spec['initial']))
            assert all(e['brightness'] == '100%' for e in ranges), ranges
            entry['colorRanges'] = ranges
            colors = [[int(c['color'][i:i + 2], 16) for i in (1, 3, 5)] for c in s['channels']]
            w, h = spec['width'], spec['height']
            xs = sorted(set(range(0, w, 7)) | {w - 1})
            ys = sorted(set(range(0, h, 7)) | {h - 1})
            points = [[x, y] for y in ys for x in xs]
            expected = [expected_pixel(spec, colors, spec['initial'], y * w + x) for x, y in points]

            def settled():
                first = readback(points)
                time.sleep(.3)
                second = readback(points, full=True)
                return second if first['hash'] == second['hash'] else False
            frame = wait(settled, 'stable rendered frame ' + name)
            assert frame['colorRenderer'] == ('webgl2' if renderer == 'gpu' else 'cpu'), frame['colorRenderer']
            assert frame['drawer'] == ('aperture-gpu' if renderer == 'gpu' else 'canvas'), frame['drawer']
            samples = frame['samples']
            assert all(samples), 'sample points outside the canvas'
            error = max(abs(a - b) for got, want in zip(samples, expected) for a, b in zip(got, want))
            entry['render'] = {k: frame[k] for k in ('width', 'height', 'ratio', 'hash', 'nonblack', 'maximum', 'colorRenderer', 'drawer')}
            entry['render'].update(points=len(points), maxErrorVsAnalytic=error)
            assert frame['nonblack'] > 0 and frame['maximum'] >= 250, entry['render']
            # 8-bit RGB uses lossy JPEG tiles, so only 16-bit asserts exact transfer.
            if sixteen:
                wrong = [(p, got, want) for p, got, want in zip(points, samples, expected)
                         if max(abs(a - b) for a, b in zip(got, want)) > 1]
                assert not wrong, (name, renderer, error, len(wrong), wrong[:8])
            results[name] = entry
            results[name]['_samples'] = samples
            results[name]['_pixels'] = frame['pixels']
            if name.startswith('gray16-low'):
                screenshot(f'gray16-low-color-panel-{renderer}')
        return results

    try:
        passes_result = {}
        for renderer in passes:
            passes_result[renderer] = run_pass(renderer)
        comparison = {}
        for spec in specs:
            name = spec['name']
            gpu, cpu = passes_result['gpu'][name], passes_result['cpu'][name]
            sampled = max(abs(a - b) for x, y in zip(gpu['_samples'], cpu['_samples']) for a, b in zip(x, y))
            g, c = base64.b64decode(gpu['_pixels']), base64.b64decode(cpu['_pixels'])
            assert (gpu['render']['width'], gpu['render']['height']) == (cpu['render']['width'], cpu['render']['height'])
            pixels = len(g) // 4
            within = sum(1 for i in range(0, len(g), 4)
                         if max(abs(g[i] - c[i]), abs(g[i + 1] - c[i + 1]), abs(g[i + 2] - c[i + 2])) <= 1)
            # OSD's canvas drawer slightly enlarges tiles to hide seams, so CPU
            # source-pixel edges may sit up to ~1.5 CSS pixels from the GPU's exact
            # geometry. The full frame allows that neighbourhood (every 4th pixel);
            # the sampled source-pixel centres above are the strict check.
            width, height = gpu['render']['width'], gpu['render']['height']
            frames = args.output.parent / 'frames'
            frames.mkdir(exist_ok=True)
            for renderer, data in (('gpu', g), ('cpu', c)):
                raw = frames / f'{name}-{renderer}.rgba'
                raw.write_bytes(data)
                subprocess.run(['vips', 'rawload', str(raw), str(raw.with_suffix('.png')), str(width), str(height), '4'],
                               check=True, capture_output=True)
                raw.unlink()
            radius = max(1, math.ceil(1.5 * gpu['render']['ratio']))
            checked = near = coverage = 0
            for y in range(radius, height - radius, 4):
                for x in range(radius, width - radius, 4):
                    at = (y * width + x) * 4
                    # Pixels only one drawer covers lie on the image's outer edge.
                    if (g[at + 3] == 0) != (c[at + 3] == 0):
                        coverage += 1
                        continue
                    checked += 1
                    near += any(max(abs(g[at + k] - c[((y + dy) * width + x + dx) * 4 + k]) for k in range(3)) <= 1
                                for dy in range(-radius, radius + 1) for dx in range(-radius, radius + 1))
            comparison[name] = {'sampledMaxDifference': sampled, 'framePixels': pixels,
                                'frameWithinOne': within, 'frameWithinOneFraction': round(within / pixels, 6),
                                'neighbourhoodRadius': radius, 'neighbourhoodChecked': checked, 'neighbourhoodWithinOne': near,
                                'neighbourhoodFraction': round(near / checked, 6), 'edgeCoverageDifferences': coverage}
            assert sampled <= 1 and near / checked >= .999, (name, comparison[name])
        report['checks']['gpuCpuComparison'] = comparison
        for renderer, results in passes_result.items():
            for entry in results.values():
                entry.pop('_samples')
                entry.pop('_pixels')
            report['checks'][renderer] = results
        for path, value in hashes.items():
            assert digest(Path(path)) == value, path
        report['checks']['sourceSha256Unchanged'] = True
        owned = app_processes() - previous_processes
        assert owned, 'native application process missing before Close'
        js("setTimeout(()=>document.getElementById('close-button').click(),100);return true;")
        deadline = time.monotonic() + args.timeout
        while app_processes() & owned and time.monotonic() < deadline:
            time.sleep(.05)
        assert not app_processes() & owned, 'normal Close left native processes running'
        report['checks']['normalCloseReapsNativeProcesses'] = True
        report['passed'] = True
    except Exception as error:
        report['error'] = repr(error)
        try:
            report['failureState'] = state()
            report['failureUi'] = ui()
            screenshot('16bit-failure')
        except Exception as capture_error:
            report['captureError'] = repr(capture_error)
        raise
    finally:
        args.output.write_text(json.dumps(report, indent=2) + '\n')
        try:
            request('DELETE', route)
        except Exception:
            pass
        # A lost WebDriver session leaves the app running with memory leases that
        # would distort later native checks; stop only processes this run started.
        for pid in app_processes() - previous_processes:
            report.setdefault('terminatedProcesses', []).append(pid)
            os.kill(pid, 15)
        if report.get('terminatedProcesses'):
            args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': report['passed'], 'checks': list(report['checks']), 'report': str(args.output)}, indent=2))


if __name__ == '__main__':
    main()
