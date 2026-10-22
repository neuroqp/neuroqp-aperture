#!/usr/bin/env python3
"""Validate ND2 max projections in an owned native Aperture session.

Build the application, then run inside scripts/headless-session --driver 4446,
which isolates cache/config directories. Only the chooser response is supplied
here. Numeric reduction is covered by Rust tests; this checks real UI, IPC,
caches and displayed pixels.
"""

import argparse
from array import array
import base64
import hashlib
import json
import os
from pathlib import Path
import sys
import time
from http.client import RemoteDisconnected
from urllib.request import Request, urlopen

import compositor

ROOT = Path(__file__).resolve().parents[1]
GIB = 1024 ** 3


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', help='defaults to the session driver; must match it')
    parser.add_argument('--fixtures', type=Path, default=ROOT / 'test-data/nd2')
    parser.add_argument('--real-z', type=Path)
    parser.add_argument('--real-time', type=Path)
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/nd2/native-projection.json')
    parser.add_argument('--timeout', type=float, default=180)
    parser.add_argument('--renderer', choices=('gpu', 'cpu'), default='gpu')
    args = parser.parse_args()
    args.driver_url = compositor.driver_url(args.driver_url)
    binary = args.binary.resolve()
    primary = args.fixtures.resolve() / 'dims_p4z5t3c2y32x32.nd2'
    real_sources = [(mode, path.resolve()) for mode, path in
                    [('z', args.real_z), ('time', args.real_time)] if path]
    hashes = {str(path): digest(path) for path in [primary] + [p for _, p in real_sources]}
    output = args.output.absolute()
    output.parent.mkdir(parents=True, exist_ok=True)
    report = {'passed': False, 'checks': {}, 'sourceHashes': hashes,
              'memorySamples': [], 'timings': [], 'screenshots': [],
              'measurementScope': 'Wall times include UI and WebDriver polling. Memory peaks are sampled explicit shared image leases, not total RSS.'}
    route = None
    original_limit = None

    def request(method, path, body=None, retry=True):
        req = Request(args.driver_url + path, method=method,
                      data=None if body is None else json.dumps(body).encode(),
                      headers={'Content-Type': 'application/json'})
        try:
            with urlopen(req, timeout=max(30, args.timeout)) as response:
                value = json.load(response).get('value')
        except (ConnectionError, RemoteDisconnected) as error:
            # WebKitWebDriver occasionally resets a connection. Retry reads and
            # scripts once, visibly; never repeat input actions.
            if not retry or not (method == 'GET' or '/execute/' in path):
                raise
            print(f'Driver connection reset, retrying {method} {path}: {error!r}', file=sys.stderr)
            time.sleep(1)
            return request(method, path, body, False)
        if isinstance(value, dict) and value.get('error') and 'message' in value:
            raise RuntimeError(value)
        return value

    def js(script, values=None, asynchronous=False):
        return request('POST', route + '/execute/' + ('async' if asynchronous else 'sync'),
                       {'script': script, 'args': values or []})

    def invoke(command, values=None):
        return js('const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(done,e=>done({error:String(e)}));',
                  [command, values or {}], True)

    def state():
        return invoke('image_state')

    def memory():
        value = invoke('memory_status')
        assert isinstance(value, dict) and 'usedBytes' in value and 'limitBytes' in value, value
        assert 0 <= value['usedBytes'] <= value['limitBytes'], value
        report['memorySamples'].append(value)
        return value

    def wait(predicate, description, sample_memory=True):
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            if sample_memory:
                memory()
            if result := predicate():
                return result
            time.sleep(.025)
        raise TimeoutError(f'{description}: {state()}')

    def click(element):
        js('document.getElementById(arguments[0]).click();', [element])

    def processes():
        found = set()
        for entry in Path('/proc').iterdir():
            if entry.name.isdigit():
                try:
                    if Path(os.readlink(entry / 'exe')) == binary:
                        found.add(int(entry.name))
                except (FileNotFoundError, PermissionError, ProcessLookupError):
                    pass
        return found

    def ready(path, mode='none', position=0, z=0, time_index=0, previous_id=None):
        def current():
            value = state()
            if not value or value.get('path') != str(path) or value.get('phase') != 'ready' or value['id'] == previous_id:
                return False
            d = value.get('dimensions') or {}
            return value if value.get('projection', 'none') == mode and (d.get('position'), d.get('zIndex'), d.get('timeIndex')) == (position, z, time_index) else False
        value = wait(current, f'ready {path.name} {mode}')
        wait(lambda: js("""
          const v=window.__apertureViewer;
          return document.getElementById('viewer').dataset.phase==='ready' && v.world.getItemCount()>0 &&
            Array.from({length:v.world.getItemCount()},(_,i)=>v.world.getItemAt(i)).every(item=>item.source.getTileUrl(item.source.maxLevel,0,0).includes('/'+arguments[0]+'/')) &&
            v.world.getItemAt(0).getTilesToDraw().some(tile=>tile.tile.loaded);
        """, [value['id']]), 'visible matching projection id')
        return value

    def open_ready(path):
        old = state()
        started = time.monotonic()
        js("window.__projectionTest.path=arguments[0];document.getElementById('open-button').click();", [str(path)])
        value = ready(path, previous_id=old['id'] if old else None)
        report['timings'].append({'path': str(path), 'operation': 'open-none', 'seconds': time.monotonic() - started})
        return value

    def axis(name):
        js("if(document.getElementById('navigator-panel').hidden)document.getElementById('navigator-button').click();const e=document.getElementById('dimension-axis');e.value=arguments[0];e.dispatchEvent(new Event('change',{bubbles:true}));", [name])

    def range_value(value):
        js("const e=document.getElementById('dimension-range');e.value=String(arguments[0]);e.dispatchEvent(new Event('input',{bubbles:true}));", [value])

    def project(mode, path=primary, position=0):
        before = state()
        started = time.monotonic()
        axis('position')
        js("for(const [id,wanted] of [['dimension-max-z',arguments[0]],['dimension-max-time',arguments[1]]]){const e=document.getElementById(id);if(e.checked!==wanted)e.click();}",
           [mode in ('z', 'zTime'), mode in ('time', 'zTime')])
        value = ready(path, mode, position=position,
                      previous_id=before['id'] if before.get('projection', 'none') != mode else None)
        report['timings'].append({'path': str(path), 'operation': mode, 'seconds': time.monotonic() - started})
        return value

    def plane_hash(value):
        return [hashlib.sha256(channel).hexdigest() for channel in preview_planes(value)]

    def samples(value):
        # 32x32 previews are full resolution; skip the 8-byte `.u16` size prefix.
        return [array('H', channel[8:]) for channel in preview_planes(value)]

    def preview_planes(value):
        data = js("""
          const done=arguments[arguments.length-1],s=arguments[0],source=window.__apertureViewer.world.getItemAt(0).source;
          const url=source.getTileUrl(source.maxLevel,0,0);
          if(!url.includes('/'+s.id+'/')){done({error:'Displayed source id mismatch'});return;}
          const base=url.split('/'+s.id+'/')[0]+'/'+s.id;
          // 16-bit planes use raw samples: u32 LE width, u32 LE height, u16 LE values.
          const ext=s.sampleBits===16?'u16':'png';
          Promise.all(s.channels.map((_,c)=>fetch(base+'/channels/'+c+'/preview.'+ext).then(r=>{
            if(!r.ok)throw new Error('Tile '+r.status);return r.arrayBuffer();
          }).then(b=>{
            const d=new DataView(b);
            if(ext==='u16'&&(b.byteLength<8||b.byteLength!==8+2*d.getUint32(0,true)*d.getUint32(4,true)))throw new Error('Invalid .u16 length');
            return Array.from(new Uint8Array(b));
          }))).then(done,e=>done({error:String(e)}));
        """, [value], True)
        assert isinstance(data, list), data
        return [bytes(channel) for channel in data]

    def framebuffer():
        value = js("""
          const done=arguments[arguments.length-1],v=window.__apertureViewer,canvas=v.drawer.canvas;
          const gl=canvas.dataset.colorRenderer==='webgl2'?canvas.getContext('webgl2'):null;
          function capture(){try{
            const w=canvas.width,h=canvas.height;let pixels;
            if(gl){pixels=new Uint8Array(w*h*4);gl.readPixels(0,0,w,h,gl.RGBA,gl.UNSIGNED_BYTE,pixels);if(gl.getError()!==gl.NO_ERROR)throw new Error('Framebuffer read failed');}
            else pixels=canvas.getContext('2d').getImageData(0,0,w,h).data;
            let hash=2166136261,nonblack=0;
            for(let i=0;i<pixels.length;i+=12){for(let c=0;c<3;c++)hash=Math.imul(hash^pixels[i+c],16777619)>>>0;if(pixels[i]||pixels[i+1]||pixels[i+2])nonblack++;}
            done({hash,nonblack,width:w,height:h});
          }catch(e){done({error:String(e)});}}
          if(gl){const timer=setTimeout(()=>{canvas.removeEventListener('aperture-color-draw',drawn);done({error:'Draw timed out'});},10000);
            function drawn(){clearTimeout(timer);canvas.removeEventListener('aperture-color-draw',drawn);capture();}
            canvas.addEventListener('aperture-color-draw',drawn);v.forceRedraw();}
          else{v.forceRedraw();requestAnimationFrame(()=>requestAnimationFrame(capture));}
        """, asynchronous=True)
        assert value.get('nonblack', 0) > 0, value
        return value

    def display():
        return js("const b=window.__apertureViewer.viewport.getBounds();return {brightness:document.getElementById('color-range-brightness')?.value,bounds:[b.x,b.y,b.width,b.height]};")

    def retained(expected):
        current = display()
        assert current['brightness'] == expected['brightness'], (expected, current)
        assert all(abs(a - b) < .000002 for a, b in zip(current['bounds'], expected['bounds'])), (expected, current)

    def no_projection_preload():
        value = wait(lambda: (p := invoke('nd2_preload_status')) and 'running' in p and not p['running'] and p, 'projected mode has no active eager preload')
        assert state().get('projection', 'none') != 'none'
        return value

    def screenshot(name):
        path = output.parent / (name + '.png')
        path.write_bytes(base64.b64decode(request('GET', route + '/screenshot')))
        report['screenshots'].append(str(path))

    try:
        previous_processes = processes()
        sid = request('POST', '/session', {'capabilities': {'alwaysMatch': {
            'browserName': 'wry', 'tauri:options': {'application': str(compositor.application(binary))},
        }}})['sessionId']
        route = f'/session/{sid}'
        output.with_suffix('.session.json').write_text(json.dumps({'sessionId': sid}) + '\n')
        url = request('GET', route + '/url').split('?')[0]
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1' + ('&colorRenderer=cpu' if args.renderer == 'cpu' else '')})
        wait(lambda: js('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;'), 'startup', False)
        js("""
          window.__projectionTest={path:null};const original=window.fetch;
          window.fetch=function(url,options){
            if(String(url).includes('plugin%3Adialog%7Copen'))
              return Promise.resolve(new Response(JSON.stringify(window.__projectionTest.path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            return original.call(this,url,options);
          };
        """)
        original_limit = memory()['limitBytes']
        reduced = invoke('set_memory_budget', {'limitBytes': GIB // 4})
        assert reduced.get('limitBytes') == GIB // 4, reduced
        source = open_ready(primary)
        assert source['sampleBits'] == 16 and source['channelMode'], source
        # Original samples of every Z/time plane at position 0, through the real UI.
        originals = {}
        for t_index in range(source['dimensions']['time']):
            for z_index in range(source['dimensions']['z']):
                axis('time')
                range_value(t_index + 1)
                axis('z')
                range_value(z_index + 1)
                originals[z_index, t_index] = samples(ready(primary, z=z_index, time_index=t_index))
        axis('time')
        range_value(1)
        axis('z')
        range_value(1)
        source = ready(primary)
        source_hash = plane_hash(source)

        def expected_maximum(mode):
            chosen = [planes for (z_index, t_index), planes in originals.items()
                      if (mode in ('z', 'zTime') or z_index == 0) and (mode in ('time', 'zTime') or t_index == 0)]
            return [array('H', map(max, *(planes[c] for planes in chosen))) for c in range(len(chosen[0]))]
        unadjusted = framebuffer()
        click('color-button')
        wait(lambda: js("return !!document.getElementById('color-range-brightness') && !document.getElementById('color-range-brightness').disabled;"), 'color brightness control')
        js("const e=document.getElementById('color-range-brightness');e.value='60';e.dispatchEvent(new Event('input',{bubbles:true}));")
        adjusted = framebuffer()
        assert adjusted['hash'] != unadjusted['hash'], (unadjusted, adjusted)
        click('zoom-in-button')
        wait(lambda: js('const p=window.__apertureViewer.viewport;return Math.abs(p.getZoom(true)-p.getZoom())<1e-8;'), 'settled zoom')
        expected_display = display()
        assert expected_display['brightness'] == '60', expected_display
        projection_hashes = {}
        for mode in ('z', 'time', 'zTime'):
            value = project(mode)
            projected_hash = plane_hash(value)
            assert projected_hash != source_hash, (mode, projected_hash, source_hash)
            # Maxima are displayed as original samples, without normalization.
            assert samples(value) == expected_maximum(mode), mode
            retained(expected_display)
            pixels = framebuffer()
            for projected_axis in ({'z': ['z'], 'time': ['time'], 'zTime': ['z', 'time']}[mode]):
                axis(projected_axis)
                assert js("return document.getElementById('dimension-range').disabled && document.getElementById('dimension-count').textContent.startsWith('Max of ');"), projected_axis
            projection_hashes[mode] = projected_hash
            report['checks'][mode] = {'hashes': projected_hash, 'exactOriginalMaxima': True, 'pixels': pixels, 'preload': no_projection_preload()}
            cleared = project('none')
            assert plane_hash(cleared) == source_hash
            retained(expected_display)
            warm = project(mode)
            assert plane_hash(warm) == projected_hash
            retained(expected_display)
        report['checks']['clearRestoresSourceAndWarmHashesMatch'] = True
        axis('position')
        range_value(2)
        moved = ready(primary, 'zTime', position=1)
        retained(expected_display)
        composite_pixels = framebuffer()
        axis('channel')
        range_value(2)
        assert state()['id'] == moved['id']
        assert js("return document.getElementById('dimension-count').textContent==='2 / 3';")
        single_pixels = framebuffer()
        assert single_pixels['hash'] != composite_pixels['hash'], (single_pixels, composite_pixels)
        range_value(1)
        assert state()['id'] == moved['id']
        assert framebuffer()['hash'] == composite_pixels['hash']
        report['checks']['positionRetainsProjectionAndChannelIsDisplayOnly'] = True
        axis('position')
        range_value(1)
        both = ready(primary, 'zTime')
        assert plane_hash(both) == projection_hashes['zTime']
        screenshot('nd2-max-projection')
        # The pinned reader exposes global Z/time counts for every position.
        # Inject a singleton metadata update through the production event path
        # to exercise the UI contract without claiming a heterogeneous-file test.
        def singleton_metadata():
            current = state()
            current['revision'] += 1
            current['dimensions'].update(z=1, time=1, zIndex=0, timeIndex=0)
            result = invoke('plugin:event|emit', {'event': 'image-state', 'payload': current})
            assert not isinstance(result, dict) or not result.get('error'), result

        singleton_metadata()
        wait(lambda: js("return ['dimension-max-z','dimension-max-time'].every(id=>{const e=document.getElementById(id);return e.checked && !e.disabled;});"), 'active singleton projections remain clearable')
        project('time')
        singleton_metadata()
        wait(lambda: js("const z=document.getElementById('dimension-max-z'),t=document.getElementById('dimension-max-time');return !z.checked && z.disabled && t.checked && !t.disabled;"), 'inactive singleton Z disabled, active Time clearable')
        cleared = project('none')
        assert plane_hash(cleared) == source_hash
        retained(expected_display)
        framebuffer()
        report['checks']['singletonMetadataAllowsClearingEachProjection'] = True
        both = project('zTime')
        # Backend-only contract phase. Reopen through the UI before rendered checks.
        canonical = invoke('select_plane', {'id': both['id'], 'position': 0, 'z': 4, 'time': 2, 'projection': 'zTime'})
        assert not canonical.get('error'), canonical
        canonical = wait(lambda: (s := state()) and s['id'] == canonical['id'] and s['phase'] == 'ready' and s, 'canonical projected cache')
        assert (canonical['dimensions']['zIndex'], canonical['dimensions']['timeIndex'], canonical['projection']) == (0, 0, 'zTime'), canonical
        assert canonical.get('detail', '').lower().startswith('reused display'), canonical
        report['checks']['projectedCoordinatesShareCanonicalDisplayCache'] = canonical['detail']
        open_ready(primary)
        axis('position')
        continuity = js("""
          const done=arguments[arguments.length-1],v=window.__apertureViewer;
          let samples=0,blank=0;const deadline=performance.now()+750;
          function sample(){samples++;if(!Array.from({length:v.world.getItemCount()},(_,i)=>v.world.getItemAt(i)).some(item=>item.getOpacity()>0 && item.getTilesToDraw().some(t=>t.tile.loaded)))blank++;
            if(performance.now()<deadline)requestAnimationFrame(sample);else done({samples,blank});}
          for(const id of ['dimension-max-z','dimension-max-time','dimension-max-z','dimension-max-time','dimension-max-z','dimension-max-time'])document.getElementById(id).click();sample();
        """, asynchronous=True)
        assert continuity['samples'] > 1 and continuity['blank'] == 0, continuity
        rapid = ready(primary, 'zTime')
        assert plane_hash(rapid) == projection_hashes['zTime']
        report['checks']['rapidTogglesCoalesceAndRetainVisibleView'] = continuity
        js("document.getElementById('dimension-max-z').click();document.getElementById('cancel-button').click();")
        wait(lambda: js("return document.getElementById('viewer').dataset.phase==='cancelled';"), 'projection cancellation')
        recovered = open_ready(primary)
        assert plane_hash(recovered) == source_hash
        framebuffer()
        report['checks']['cancellationAndReplacementRecover'] = True
        for mode, path in real_sources:
            restored = invoke('set_memory_budget', {'limitBytes': max(4 * GIB, original_limit)})
            assert restored.get('limitBytes') == max(4 * GIB, original_limit), restored
            real_source = open_ready(path)
            real_source_hash = plane_hash(real_source)
            projected = project(mode, path)
            cold_hash = plane_hash(projected)
            assert cold_hash != real_source_hash, (path, cold_hash, real_source_hash)
            cold_pixels = framebuffer()
            project('none', path)
            assert plane_hash(state()) == real_source_hash
            warm = project(mode, path)
            assert plane_hash(warm) == cold_hash
            report['checks']['real-' + mode] = {'path': str(path), 'sourceHashes': real_source_hash, 'projectionHashes': cold_hash, 'pixels': cold_pixels, 'memory': memory(), 'preload': no_projection_preload()}
        for path, source_hash in hashes.items():
            assert digest(Path(path)) == source_hash, path
        report['checks']['sourceSha256Unchanged'] = True
        report['sampledTrackedImagePeakBytes'] = max(s['usedBytes'] for s in report['memorySamples'])
        restored = invoke('set_memory_budget', {'limitBytes': original_limit})
        assert restored.get('limitBytes') == original_limit, restored
        owned = processes() - previous_processes
        assert owned, 'owned native process missing before Close'
        js("setTimeout(()=>document.getElementById('close-button').click(),100);return true;")
        wait(lambda: not processes() & owned, 'normal Close reaps native processes', False)
        report['checks']['normalCloseReapsNativeProcesses'] = True
        report['passed'] = True
    except Exception as error:
        report['error'] = repr(error)
        if route:
            try:
                report['failureState'] = state()
                report['failureMemory'] = invoke('memory_status')
                screenshot('nd2-projection-failure')
            except Exception as capture_error:
                report['captureError'] = repr(capture_error)
        raise
    finally:
        if route:
            try:
                if not report['passed'] and original_limit is not None:
                    invoke('set_memory_budget', {'limitBytes': original_limit})
                request('DELETE', route)
            except Exception as cleanup_error:
                report['cleanupError'] = repr(cleanup_error)
        output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': report['passed'], 'checks': list(report['checks']), 'report': str(output)}, indent=2))


if __name__ == '__main__':
    main()
