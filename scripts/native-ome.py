#!/usr/bin/env python3
"""Validate OME-TIFF browsing in an owned native Aperture session (stdlib WebDriver).

Build the application, then run inside scripts/headless-session --driver 4446.
The script writes small deterministic OME-TIFFs to the ignored test-data/ome/
and checks, through the real UI:

- the real GTK Open chooser opens an OME-TIFF and a `.companion.ome`;
- Position/Z/Time navigation in the Dimensions panel shows exactly the selected
  plane in `.u16` (16-bit) or PNG (8-bit) previews and full-resolution tiles,
  and Channel lists the OME channel names;
- multi-file BinaryOnly sets resolve every member, also via the companion;
- an unstored plane reports a recoverable error and navigation continues;
- Z max, Time max and both equal maxima computed here; a frame above 256 MiB
  disables projections with the reason shown in the panel;
- skipped series are noted in the status bar; OME-TIFF is browsing only;
- the chosen renderer draws (GPU readback or CPU canvas); sources stay unchanged.

Optional: --public DIR compares planes of public OME-TIFF samples found there
against libvips page reads. A JSON report is written under artifacts/ome/.
"""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import time
import zlib
from http.client import RemoteDisconnected
from urllib.request import Request, urlopen

import compositor

ROOT = Path(__file__).resolve().parents[1]
TILE = 256
NS = 'http://www.openmicroscopy.org/Schemas/OME/2016-06'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_tiff(path, pages, description=None):
    """Uncompressed little-endian TIFF. Pages are (width, height, bits, samples, values)."""
    out = bytearray(b'II*\x00\x00\x00\x00\x00')
    link = 4
    for index, (width, height, bits, samples, values) in enumerate(pages):
        if len(out) % 2:
            out.append(0)
        data = len(out)
        out += struct.pack(f'<{len(values)}{"H" if bits == 16 else "B"}', *values)
        extra = {}
        if len(out) % 2:
            out.append(0)
        if samples > 1:
            extra['bits'] = len(out)
            out += struct.pack(f'<{samples}H', *([bits] * samples))
        text = description.encode() + b'\x00' if description and index == 0 else None
        if text:
            if len(out) % 2:
                out.append(0)
            extra['text'] = len(out)
            out += text
        entries = [(256, 4, 1, width), (257, 4, 1, height),
                   (258, 3, samples, extra['bits'] if samples > 1 else bits),
                   (259, 3, 1, 1), (262, 3, 1, 2 if samples == 3 else 1)]
        if text:
            entries.append((270, 2, len(text), extra['text']))
        entries += [(273, 4, 1, data), (277, 3, 1, samples), (278, 4, 1, height),
                    (279, 4, 1, width * height * samples * bits // 8), (284, 3, 1, 1)]
        if len(out) % 2:
            out.append(0)
        out[link:link + 4] = struct.pack('<I', len(out))
        out += struct.pack('<H', len(entries))
        for tag, kind, count, value in entries:
            inline = struct.pack('<HH', value, 0) if kind == 3 and count == 1 else struct.pack('<I', value)
            out += struct.pack('<HHI', tag, kind, count) + inline
        link = len(out)
        out += b'\x00\x00\x00\x00'
    path.write_bytes(bytes(out))


def ome(uuid, body):
    return f'<?xml version="1.0" encoding="UTF-8"?><OME xmlns="{NS}" UUID="{uuid}">{body}</OME>'


def image(name, kind, size, czt, children, extra=''):
    (w, h), (c, z, t) = size, czt
    return (f'<Image ID="Image:{name}" Name="{name}"><Pixels ID="Pixels:{name}" DimensionOrder="XYCZT" '
            f'Type="{kind}" SizeX="{w}" SizeY="{h}" SizeC="{c}" SizeZ="{z}" SizeT="{t}" '
            f'PhysicalSizeX="0.5" PhysicalSizeY="0.5" PhysicalSizeZ="2" TimeIncrement="3" {extra}>'
            f'{children}</Pixels></Image>')


def plane(width, height, bits, seed, samples=1):
    """Distinct samples per plane; 16-bit values exercise both bytes."""
    if bits == 8:
        return [(seed * 37 + x * 3 + y * 5 + s * 61) % 256
                for y in range(height) for x in range(width) for s in range(samples)]
    return [(seed * 4099 + x * 131 + y * 517 + s * 9973) % 60000 + 100
            for y in range(height) for x in range(width) for s in range(samples)]


def fixtures(directory):
    """Write the datasets; each expected plane is a flat list per (position, c, z, t)."""
    directory.mkdir(parents=True, exist_ok=True)
    sets = {}
    # Two positions x 2C x 3Z x 2T, 16-bit, plus a skipped 8-bit label image.
    size = (97, 61)
    pages, expected = [], {}
    for position in range(2):
        for t in range(2):
            for z in range(3):
                for c in range(2):
                    seed = len(pages)
                    values = plane(*size, 16, seed)
                    expected[(position, c, z, t)] = values
                    pages.append((*size, 16, 1, values))
    pages.append((4, 4, 8, 1, [7] * 16))
    channels = '<Channel Name="DAPI" Color="65535"/><Channel Name="GFP" Color="16711935"/>'
    xml = ome('urn:uuid:single', ''.join(
        image(name, 'uint16', size, (2, 3, 2), channels + f'<TiffData IFD="{12 * index}" PlaneCount="12"/>')
        for index, name in enumerate(['Well A1', 'Well A2'])) + image('Label', 'uint8', (4, 4), (1, 1, 1), '<TiffData IFD="24"/>'))
    path = directory / 'cells-16bit.ome.tif'
    write_tiff(path, pages, xml)
    sets['single'] = {'path': path, 'bits': 16, 'size': size, 'planes': expected, 'positions': 2,
                      'czt': (2, 3, 2), 'channels': ['DAPI', 'GFP'], 'skipped': 1}
    # Plain .tif name, 8-bit, 1C x 2Z.
    size8 = (45, 33)
    expected8 = {(0, 0, z, 0): plane(*size8, 8, 50 + z) for z in range(2)}
    path = directory / 'cells-8bit.tif'
    write_tiff(path, [(*size8, 8, 1, expected8[(0, 0, z, 0)]) for z in range(2)],
               ome('urn:uuid:eight', image('Eight', 'uint8', size8, (1, 2, 1), '<TiffData PlaneCount="2"/>')))
    sets['eight'] = {'path': path, 'bits': 8, 'size': size8, 'planes': expected8, 'positions': 1,
                     'czt': (1, 2, 1), 'channels': ['Channel 1'], 'skipped': 0}
    # Multi-file BinaryOnly set with a companion: Z 1-2 in a, Z 3-4 in b.
    sizem = (40, 30)
    expectedm = {(0, 0, z, 0): plane(*sizem, 16, 200 + z) for z in range(4)}
    data = ('<TiffData FirstZ="0" PlaneCount="2"><UUID FileName="set-a.ome.tif">urn:uuid:set-a</UUID></TiffData>'
            '<TiffData FirstZ="2" PlaneCount="2"><UUID FileName="set-b.ome.tif">urn:uuid:set-b</UUID></TiffData>')
    companion = directory / 'set.companion.ome'
    companion.write_text(ome('urn:uuid:set-meta', image('Set', 'uint16', sizem, (1, 4, 1), data)))
    for name, zs in (('set-a', (0, 1)), ('set-b', (2, 3))):
        write_tiff(directory / f'{name}.ome.tif', [(*sizem, 16, 1, expectedm[(0, 0, z, 0)]) for z in zs],
                   ome(f'urn:uuid:{name}', '<BinaryOnly MetadataFile="set.companion.ome" UUID="urn:uuid:set-meta"/>'))
    sets['multi'] = {'path': companion, 'member': directory / 'set-a.ome.tif', 'bits': 16, 'size': sizem,
                     'planes': expectedm, 'positions': 1, 'czt': (1, 4, 1), 'channels': ['Channel 1'], 'skipped': 0}
    # Z 2 of 3 is not stored.
    sizeg = (21, 17)
    expectedg = {(0, 0, 0, 0): plane(*sizeg, 16, 300), (0, 0, 2, 0): plane(*sizeg, 16, 302)}
    path = directory / 'gaps.ome.tif'
    write_tiff(path, [(*sizeg, 16, 1, expectedg[(0, 0, 0, 0)]), (*sizeg, 16, 1, expectedg[(0, 0, 2, 0)])],
               ome('urn:uuid:gaps', image('Gaps', 'uint16', sizeg, (1, 3, 1),
                                          '<TiffData IFD="0"/><TiffData IFD="1" FirstZ="2"/>')))
    sets['gaps'] = {'path': path, 'bits': 16, 'size': sizeg, 'planes': expectedg, 'positions': 1,
                    'czt': (1, 3, 1), 'channels': ['Channel 1'], 'skipped': 0}
    # RGB16 over Z and T.
    sizer = (33, 19)
    expectedr = {}
    pages = []
    for t in range(2):
        for z in range(2):
            values = plane(*sizer, 16, 400 + z + 2 * t, 3)
            pages.append((*sizer, 16, 3, values))
            for band in range(3):
                expectedr[(0, band, z, t)] = values[band::3]
    path = directory / 'rgb16.ome.tif'
    write_tiff(path, pages, ome('urn:uuid:rgb', image('RGB', 'uint16', sizer, (3, 2, 2),
                                                       '<Channel SamplesPerPixel="3"/><TiffData PlaneCount="4"/>')))
    sets['rgb'] = {'path': path, 'bits': 16, 'size': sizer, 'planes': expectedr, 'positions': 1,
                   'czt': (3, 2, 2), 'rgb': True, 'channels': ['Red', 'Green', 'Blue'], 'skipped': 0}
    # 32 channels sharing one 2049 x 2048 page: a 268 MB frame cannot be projected.
    sizel = (2049, 2048)
    path = directory / 'large-frame.ome.tif'
    data = ''.join(f'<TiffData IFD="0" FirstC="{c}" FirstZ="{z}" PlaneCount="1"/>' for z in range(2) for c in range(32))
    write_tiff(path, [(*sizel, 16, 1, [(x + y) % 4096 for y in range(sizel[1]) for x in range(sizel[0])])],
               ome('urn:uuid:large', image('Large', 'uint16', sizel, (32, 2, 1), data)))
    sets['large'] = {'path': path}
    for value in sets.values():
        value['path'] = value['path'].resolve()
    return sets


def png_samples(data):
    """Decode an 8-bit grayscale, non-interlaced PNG."""
    assert data[:8] == b'\x89PNG\r\n\x1a\n'
    at, chunks = 8, {}
    raw = b''
    while at < len(data):
        length, kind = struct.unpack('>I4s', data[at:at + 8])
        body = data[at + 8:at + 8 + length]
        if kind == b'IHDR':
            chunks['header'] = struct.unpack('>IIBBBBB', body)
        elif kind == b'IDAT':
            raw += body
        at += 12 + length
    width, height, depth, color, _, _, interlace = chunks['header']
    assert (depth, color, interlace) == (8, 0, 0), chunks['header']
    stream = zlib.decompress(raw)
    rows, previous = [], bytearray(width)
    for y in range(height):
        kind, line = stream[y * (width + 1)], bytearray(stream[y * (width + 1) + 1:(y + 1) * (width + 1)])
        for x in range(width):
            a = line[x - 1] if x else 0
            b, c = previous[x], previous[x - 1] if x else 0
            if kind == 1:
                line[x] = (line[x] + a) & 255
            elif kind == 2:
                line[x] = (line[x] + b) & 255
            elif kind == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(line)
        previous = line
    return width, height, [v for row in rows for v in row]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', help='defaults to the session driver; must match it')
    parser.add_argument('--fixtures', type=Path, default=ROOT / 'test-data/ome')
    parser.add_argument('--public', type=Path, help='directory with public OME-TIFF samples')
    parser.add_argument('--renderer', choices=('gpu', 'cpu'), default='gpu')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--timeout', type=float, default=120)
    args = parser.parse_args()
    args.driver_url = compositor.driver_url(args.driver_url)
    desktop = compositor.session()
    args.output = (args.output or ROOT / f'artifacts/ome/native-ome-{args.renderer}.json').absolute()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    binary = args.binary.resolve()
    sets = fixtures(args.fixtures.resolve())
    files = sorted(p for p in args.fixtures.resolve().iterdir() if p.is_file())
    hashes = {str(p): digest(p) for p in files}
    report = {'passed': False, 'renderer': args.renderer, 'checks': {}, 'sourceHashes': hashes, 'screenshots': []}

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
        return js('const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(done,e=>done({error:String(e)}));',
                  [command, values or {}], True)

    def state():
        return invoke('image_state')

    def ui():
        return js("""const g=id=>document.getElementById(id);return {phase:g('viewer').dataset.phase,name:g('filename').textContent,
          status:g('status').textContent,statusError:g('status').classList.contains('error'),count:g('dimension-count').textContent,
          dimensionName:g('dimension-name').textContent,projectionStatus:g('dimension-projection-status').hidden?null:g('dimension-projection-status').textContent,
          maxZDisabled:g('dimension-max-z').disabled,maxTimeDisabled:g('dimension-max-time').disabled,scope:!g('dimension-scope').hidden,
          pointsDisabled:g('points-button').disabled,saveDisabled:g('save-button').disabled,items:window.__apertureViewer.world.getItemCount()};""")

    def wait(predicate, description, timeout=None):
        deadline = time.monotonic() + (timeout or args.timeout)
        while time.monotonic() < deadline:
            if result := predicate():
                return result
            time.sleep(.05)
        raise TimeoutError(f'{description}: {state()} {ui()}')

    def screenshot(name):
        path = args.output.parent / f'{name}-{args.renderer}.png'
        path.write_bytes(base64.b64decode(request('GET', route + '/screenshot')))
        report['screenshots'].append(str(path))
        if os.environ.get('APERTURE_SESSION') == 'headless':
            session_path = path.with_suffix('.session.png')
            desktop.screenshot(session_path)
            report['screenshots'].append(str(session_path))

    def app_window(title=None):
        owned = app_processes() - previous_processes
        return next((w for w in desktop.windows() if w['pid'] in owned and w['app_id'] == compositor.APP_ID
                     and (w['title'] == title if title else w['title'] != 'Open File')), None)

    def chooser_open(path):
        """Select `path` in the real GTK file chooser (nested session input only)."""
        js("document.getElementById('open-button').click();return true;")
        dialog = wait(lambda: app_window('Open File'), 'native chooser', 15)
        desktop.focus(dialog)
        time.sleep(.3)
        desktop.keys('ctrl+l')
        time.sleep(.5)
        desktop.set_clipboard(str(path).encode())
        desktop.keys('ctrl+a')
        desktop.keys('ctrl+v')
        time.sleep(1)
        desktop.keys('Return')
        time.sleep(.5)
        if (dialog := app_window('Open File')) and dialog['focused']:
            desktop.keys('Return')
        wait(lambda: not app_window('Open File'), 'chooser closed', 15)

    def stub_open(path):
        js("window.__omeTest.path=arguments[0];document.getElementById('open-button').click();", [str(path)])

    def start():
        url = request('GET', route + '/url').split('?')[0]
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1' + ('&colorRenderer=cpu' if args.renderer == 'cpu' else '')})
        wait(lambda: js('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;'), 'app startup')
        window = wait(lambda: app_window(), 'app window')
        desktop.resize(window, 1280, 900)
        js("""window.__omeTest={path:null};const original=window.fetch;
          window.fetch=function(url,options){
            if(window.__omeTest.path&&String(url).includes('plugin%3Adialog%7Copen')){const path=window.__omeTest.path;window.__omeTest.path=null;
              return Promise.resolve(new Response(JSON.stringify(path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));}
            return original.call(this,url,options);};""")

    def displayed(s):
        """The backend selection is ready and every viewer item draws it."""
        return js("""const v=window.__apertureViewer,n=v.world.getItemCount();
          return document.getElementById('viewer').dataset.phase==='ready' && n>0 &&
            Array.from({length:n},(_,i)=>v.world.getItemAt(i)).every(item=>item.source.getTileUrl(item.source.maxLevel,0,0).includes('/'+arguments[0]+'/') && item.getFullyLoaded());""", [s['id']])

    def ready(path, previous_id, expect_error=False, projection=None):
        def current():
            s = state()
            if not s or s.get('path') != str(path) or s['id'] == previous_id:
                return False
            # Toggling two checkboxes passes through an intermediate selection.
            if projection and s.get('projection') != projection:
                return False
            if s['phase'] == 'error' and not expect_error:
                raise RuntimeError(s.get('error') or s)
            return s if s['phase'] == ('error' if expect_error else 'ready') else False
        s = wait(current, f'{"error" if expect_error else "ready"} {path.name}')
        if not expect_error:
            try:
                wait(lambda: displayed(s), 'displayed ' + path.name)
            except TimeoutError as error:
                raise TimeoutError(str(error), js("""const v=window.__apertureViewer;
                  return Array.from({length:v.world.getItemCount()},(_,i)=>{const item=v.world.getItemAt(i);
                    return {url:item.source.getTileUrl(item.source.maxLevel,0,0),opacity:item.getOpacity(),fully:item.getFullyLoaded()};});""")) from error
        return s

    def open_ready(path, chooser=False):
        previous = state()
        (chooser_open if chooser else stub_open)(path)
        return ready(path, previous['id'] if previous else None)

    def axis(name):
        js("if(document.getElementById('navigator-panel').hidden)document.getElementById('navigator-button').click();const e=document.getElementById('dimension-axis');e.value=arguments[0];e.dispatchEvent(new Event('change',{bubbles:true}));", [name])

    def navigate(name, index, path, expect_error=False):
        """Select a 0-based index on an axis through the panel's range input."""
        previous = state()['id']
        axis(name)
        js("const e=document.getElementById('dimension-range');e.value=String(arguments[0]);e.dispatchEvent(new Event('input',{bubbles:true}));", [index + 1])
        return ready(path, previous, expect_error)

    def project(mode, path):
        previous = state()['id']
        js("for(const [id,wanted] of [['dimension-max-z',arguments[0]],['dimension-max-time',arguments[1]]]){const e=document.getElementById(id);if(e.checked!==wanted)e.click();}",
           [mode in ('z', 'zTime'), mode in ('time', 'zTime')])
        return ready(path, previous, projection=mode)

    def fetch(s, paths):
        result = js("""const done=arguments[arguments.length-1],s=arguments[0],paths=arguments[1];
          const source=window.__apertureViewer.world.getItemAt(0).source,url=source.getTileUrl(source.maxLevel,0,0);
          if(!url.includes('/'+s.id+'/')){done({error:'Displayed source id differs from backend'});return;}
          const base=url.split('/'+s.id+'/')[0]+'/'+s.id;
          const encode=b=>{let t='';for(let i=0;i<b.length;i+=32768)t+=String.fromCharCode.apply(null,b.subarray(i,i+32768));return btoa(t);};
          Promise.all(paths.map(p=>fetch(base+p).then(async r=>({status:r.status,type:r.headers.get('content-type'),
            data:r.ok?encode(new Uint8Array(await r.arrayBuffer())):''})))).then(done,e=>done({error:String(e)}));""", [s, paths], True)
        assert isinstance(result, list), result
        assert all(r['status'] == 200 for r in result), [(p, r['status']) for p, r in zip(paths, result)]
        return [base64.b64decode(r['data']) for r in result]

    def decode(body, bits):
        if bits == 16:
            width, height = struct.unpack('<II', body[:8])
            assert len(body) == 8 + 2 * width * height
            return width, height, list(struct.unpack(f'<{width * height}H', body[8:]))
        return png_samples(body)

    def check_planes(s, spec, position, z, t):
        """Previews and every full-resolution tile equal the selected planes."""
        w, h = spec['size']
        bits = spec['bits']
        suffix = 'u16' if bits == 16 else 'png'
        channels = spec['czt'][0]
        level = (max(w, h) - 1).bit_length()
        columns, rows = -(-w // TILE), -(-h // TILE)
        paths = [f'/channels/{c}/preview.{suffix}' for c in range(channels)]
        paths += [f'/channels/{c}/tiles/{level}/{x}_{y}.{suffix}' for c in range(channels)
                  for y in range(rows) for x in range(columns)]
        bodies = iter(fetch(s, paths))
        for c in range(channels):
            pw, ph, preview = decode(next(bodies), bits)
            assert (pw, ph) == (w, h) and preview == spec['planes'][(position, c, z, t)], (spec['path'].name, c, z, t)
        for c in range(channels):
            stitched = [None] * (w * h)
            for y in range(rows):
                for x in range(columns):
                    tw, th, tile = decode(next(bodies), bits)
                    for row in range(th):
                        at = (y * TILE + row) * w + x * TILE
                        stitched[at:at + tw] = tile[row * tw:(row + 1) * tw]
            assert stitched == spec['planes'][(position, c, z, t)], (spec['path'].name, c, 'tiles differ')
        return {'position': position, 'z': z, 't': t, 'channels': channels, 'tiles': rows * columns * channels}

    def rendered():
        result = js("""const done=arguments[arguments.length-1],v=window.__apertureViewer,canvas=v.drawer.canvas;
          const gl=canvas.dataset.colorRenderer==='webgl2'?canvas.getContext('webgl2'):null;
          function capture(){const w=canvas.width,h=canvas.height;let px;
            if(gl){px=new Uint8Array(w*h*4);gl.readPixels(0,0,w,h,gl.RGBA,gl.UNSIGNED_BYTE,px);}
            else px=canvas.getContext('2d').getImageData(0,0,w,h).data;
            let nonblack=0;for(let i=0;i<px.length;i+=4)if(px[i]||px[i+1]||px[i+2])nonblack++;
            done({renderer:canvas.dataset.colorRenderer,drawer:v.drawer.getType(),nonblack});}
          if(gl){const drawn=()=>{canvas.removeEventListener('aperture-color-draw',drawn);capture();};
            canvas.addEventListener('aperture-color-draw',drawn);v.forceRedraw();}
          else{v.forceRedraw();requestAnimationFrame(()=>requestAnimationFrame(capture));}""", [], True)
        assert result['renderer'] == ('webgl2' if args.renderer == 'gpu' else 'cpu'), result
        assert result['nonblack'] > 0, result
        return result

    def basics(s, spec):
        assert s['readOnly'] and s['channelMode'] and s['sampleBits'] == spec['bits'], s
        assert [c['name'] for c in s['channels']] == spec['channels'], s['channels']
        assert len(s['dimensions']['positions']) == spec['positions'], s['dimensions']
        assert (s['dimensions']['z'], s['dimensions']['time']) == spec['czt'][1:], s['dimensions']
        assert s['skippedSeries'] == spec['skipped'], s
        view = wait(lambda: (u := ui()) and u['scope'] and u['pointsDisabled'] and u['saveDisabled'] and u, 'browsing-only UI')
        if spec['skipped']:
            assert f"{spec['skipped']} series with different dimensions not shown" in view['status'], view
        denied = invoke('load_annotations', {'id': s['id']})
        assert 'browsing-only' in str(denied.get('error', '')), denied
        return view

    try:
        start()
        checks = report['checks']
        filters = invoke('image_extensions')
        assert 'companion.ome' in filters and 'ome' not in filters, filters
        checks['openFilter'] = filters

        # Single file: real chooser, then Position/Z/Time/Channel navigation.
        spec = sets['single']
        s = open_ready(spec['path'], chooser=True)
        entry = {'basics': basics(s, spec), 'planes': [check_planes(s, spec, 0, 0, 0)]}
        for name, index, selection in (('z', 2, (0, 2, 0)), ('time', 1, (0, 2, 1)), ('position', 1, (1, 2, 1)), ('z', 0, (1, 0, 1))):
            s = navigate(name, index, spec['path'])
            d = s['dimensions']
            assert (d['position'], d['zIndex'], d['timeIndex']) == selection, d
            entry['planes'].append(check_planes(s, spec, *selection))
        axis('position')
        assert ui()['dimensionName'] == 'Well A2', ui()
        axis('channel')
        js("const e=document.getElementById('dimension-range');e.value='3';e.dispatchEvent(new Event('input',{bubbles:true}));")
        assert wait(lambda: ui()['dimensionName'] == 'GFP', 'channel GFP'), ui()
        axis('position')
        entry['render'] = rendered()
        screenshot('single-plane')
        # Projections at position 2, T 2 equal maxima computed here.
        projections = {}
        for mode, zs, ts in (('z', range(3), [1]), ('time', [0], range(2)), ('zTime', range(3), range(2))):
            s = project(mode, spec['path'])
            assert s['projection'] == mode, s
            bodies = fetch(s, [f'/channels/{c}/preview.u16' for c in range(2)])
            for c, body in enumerate(bodies):
                _, _, values = decode(body, 16)
                maxima = [max(spec['planes'][(1, c, z, t)][i] for z in zs for t in ts) for i in range(len(values))]
                assert values == maxima, (mode, c)
                assert s['displayRanges'][c] == [min(maxima), max(maxima)], s['displayRanges']
            projections[mode] = {'exact': True, 'ui': ui()['projectionStatus']}
        screenshot('projection-z-time')
        s = project('none', spec['path'])
        entry['projections'] = projections
        checks['single'] = entry

        # Plain .tif name, 8-bit PNG planes.
        spec = sets['eight']
        s = open_ready(spec['path'])
        basics(s, spec)
        checks['eightBit'] = [check_planes(s, spec, 0, 0, 0)]
        s = navigate('z', 1, spec['path'])
        checks['eightBit'].append(check_planes(s, spec, 0, 1, 0))

        # Multi-file set through the companion (real chooser) and a member.
        spec = sets['multi']
        multi = {}
        for opened, chooser in ((spec['path'], True), (spec['member'], False)):
            s = open_ready(opened, chooser=chooser)
            basics(s, spec)
            planes = []
            for z in (3, 1):
                s = navigate('z', z, opened)
                planes.append(check_planes(s, spec, 0, z, 0))
            multi[opened.name] = planes
        checks['multiFile'] = multi

        # An unstored plane fails only its selection.
        spec = sets['gaps']
        s = open_ready(spec['path'])
        failed = navigate('z', 1, spec['path'], expect_error=True)
        assert failed['error'] == 'OME-TIFF plane C 1, Z 2, T 1 is not stored in this dataset.', failed
        view = wait(lambda: (u := ui()) and u['statusError'] and 'not stored' in u['status'] and u['items'] == 0 and u, 'not-stored status')
        screenshot('not-stored-plane')
        s = navigate('z', 2, spec['path'])
        checks['notStored'] = {'error': failed['error'], 'ui': view, 'recovered': check_planes(s, spec, 0, 2, 0)}

        # RGB16 across Z and time.
        spec = sets['rgb']
        s = open_ready(spec['path'])
        basics(s, spec)
        assert s['rgb']
        rgb = [check_planes(s, spec, 0, 0, 0)]
        s = navigate('time', 1, spec['path'])
        s = navigate('z', 1, spec['path'])
        rgb.append(check_planes(s, spec, 0, 1, 1))
        checks['rgb16'] = rgb

        # A frame above 256 MiB browses but cannot be projected.
        large = sets['large']['path']
        previous = state()['id']
        stub_open(large)
        s = wait(lambda: (v := state()) and v['path'] == str(large) and v['id'] != previous and v['previewReady'] and v, 'large preview')
        assert s['projectionUnavailable'] and '256 MiB' in s['projectionUnavailable'], s
        view = wait(lambda: (u := ui()) and u['maxZDisabled'] and u['maxTimeDisabled'] and u['projectionStatus'] == s['projectionUnavailable'] and u,
                    'projection reason')
        screenshot('projection-unavailable')
        checks['projectionUnavailable'] = view

        if args.public:
            checks['public'] = public_samples(args.public, open_ready, navigate, fetch, decode, state)

        for path, value in hashes.items():
            assert digest(Path(path)) == value, path
        checks['sourcesUnchanged'] = True
        owned = app_processes() - previous_processes
        js("setTimeout(()=>document.getElementById('close-button').click(),100);return true;")
        deadline = time.monotonic() + args.timeout
        while app_processes() & owned and time.monotonic() < deadline:
            time.sleep(.05)
        assert not app_processes() & owned, 'normal Close left native processes running'
        checks['normalClose'] = True
        report['passed'] = True
    except Exception as error:
        report['error'] = repr(error)
        try:
            report['failureState'] = state()
            report['failureUi'] = ui()
            screenshot('ome-failure')
        except Exception as capture_error:
            report['captureError'] = repr(capture_error)
        raise
    finally:
        args.output.write_text(json.dumps(report, indent=2, default=str) + '\n')
        try:
            request('DELETE', route)
        except Exception:
            pass
        for pid in app_processes() - previous_processes:
            report.setdefault('terminatedProcesses', []).append(pid)
            os.kill(pid, 15)
        if report.get('terminatedProcesses'):
            args.output.write_text(json.dumps(report, indent=2, default=str) + '\n')
    print(json.dumps({'passed': report['passed'], 'checks': list(report['checks']), 'report': str(args.output)}, indent=2))


def vips_plane(path, page, width, height):
    """Samples of one page read independently through the libvips tools."""
    raw = Path(os.environ.get('TMPDIR', '/tmp')) / f'ome-page-{os.getpid()}.raw'
    subprocess.run(['vips', 'rawsave', f'{path}[page={page},n=1]', str(raw)], check=True, capture_output=True)
    data = raw.read_bytes()
    raw.unlink()
    if len(data) == width * height * 2:
        return list(struct.unpack(f'<{width * height}H', data))
    return list(data)


def public_samples(directory, open_ready, navigate, fetch, decode, state):
    """Compare a few planes of public samples with libvips reads of their pages."""
    results = {}
    for name in ('multi-channel-4D-series.ome.tif', 'tubhiswt-3D/tubhiswt_C0.ome.tif',
                 'companion/multifile.companion.ome', 'binaryonly/multifile-Z2.ome.tiff'):
        path = (directory / name).resolve()
        if not path.is_file():
            results[name] = 'not present'
            continue
        try:
            s = open_ready(path)
        except RuntimeError as error:
            # Unsupported content must fail with a specific message (e.g. int8 pixels).
            results[name] = {'expectedError': str(error)}
            assert str(error).startswith('OME-TIFF: '), error
            continue
        width, height = s['width'], s['height']
        dims = s['dimensions']
        checked = []
        for z, t in ((0, 0), (dims['z'] - 1, dims['time'] - 1)):
            if s['dimensions']['zIndex'] != z:
                s = navigate('z', z, path)
            if s['dimensions']['timeIndex'] != t:
                s = navigate('time', t, path)
            bits = s['sampleBits']
            suffix = 'u16' if bits == 16 else 'png'
            bodies = fetch(s, [f'/channels/{c}/preview.{suffix}' for c in range(len(s['channels']))])
            pages = invoke_pages(s, path, len(s['channels']))
            for c, body in enumerate(bodies):
                pw, ph, values = decode(body, bits)
                if (pw, ph) != (width, height):
                    continue  # Larger planes are reduced previews.
                member, page = pages[c]
                assert values == vips_plane(member, page, width, height), (name, c, z, t)
                checked.append({'c': c, 'z': z, 't': t, 'file': member.name, 'page': page})
        results[name] = {'width': width, 'height': height, 'dimensions': dims, 'checked': checked}
    return results


def first_description(path):
    """ImageDescription of the first IFD of a classic or BigTIFF file."""
    data = path.read_bytes()
    order = '<' if data[:2] == b'II' else '>'
    big = struct.unpack(order + 'H', data[2:4])[0] == 43
    if big:
        at = struct.unpack(order + 'Q', data[8:16])[0]
        count, at = struct.unpack(order + 'Q', data[at:at + 8])[0], at + 8
    else:
        at = struct.unpack(order + 'I', data[4:8])[0]
        count, at = struct.unpack(order + 'H', data[at:at + 2])[0], at + 2
    size = 20 if big else 12
    for index in range(count):
        entry = data[at + index * size:at + (index + 1) * size]
        if struct.unpack(order + 'H', entry[:2])[0] == 270:
            length = struct.unpack(order + ('Q' if big else 'I'), entry[4:12 if big else 8])[0]
            value = entry[12:20] if big else entry[8:12]
            offset = struct.unpack(order + ('Q' if big else 'I'), value)[0]
            text = value[:length] if length <= len(value) else data[offset:offset + length]
            return text.rstrip(b'\0').decode()
    raise ValueError(f'{path} has no ImageDescription')


def invoke_pages(s, path, channels):
    """Member file and page of each channel, from the OME-XML the app also reads."""
    import xml.etree.ElementTree as ET  # Trusted, small public metadata only.
    def metadata(source):
        return source.read_text() if source.suffix == '.ome' else first_description(source)
    root = ET.fromstring(metadata(path))
    binary = next((e for e in root if e.tag.endswith('BinaryOnly')), None)
    if binary is not None:
        root = ET.fromstring(metadata(path.parent / binary.get('MetadataFile')))
    ns = {'o': root.tag[1:].split('}')[0]}
    pixels = root.findall('o:Image', ns)[s['dimensions']['position']].find('o:Pixels', ns)
    order = pixels.get('DimensionOrder')[2:]
    sizes = {'C': int(pixels.get('SizeC')), 'Z': int(pixels.get('SizeZ')), 'T': int(pixels.get('SizeT'))}
    z, t = s['dimensions']['zIndex'], s['dimensions']['timeIndex']
    result = []
    for c in range(channels):
        index = {'C': c, 'Z': z, 'T': t}
        linear, stride = 0, 1
        for axis in order:
            linear += index[axis] * stride
            stride *= sizes[axis]
        for data in pixels.findall('o:TiffData', ns):
            first = sum(int(data.get('First' + a, 0)) * s_ for a, s_ in zip(order, [1, sizes[order[0]], sizes[order[0]] * sizes[order[1]]]))
            count = int(data.get('PlaneCount', 1 if data.get('IFD') else 1 << 30))
            if first <= linear < first + count:
                uuid = data.find('o:UUID', ns)
                member = path.parent / uuid.get('FileName') if uuid is not None and uuid.get('FileName') else path
                result.append((member, int(data.get('IFD', 0)) + linear - first))
                break
    return result


if __name__ == '__main__':
    main()
