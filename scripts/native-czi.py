#!/usr/bin/env python3
"""Validate Zeiss CZI in an owned native Aperture session using stdlib WebDriver.

Parent orchestration starts tauri-driver. Only the chooser result is supplied;
real UI handlers, Rust IPC, CZI reader, cache and tile protocol handle the data.
No Python CZI decoder is used. expected.json (czifile reference values) is read
as plain data for geometry. Fixtures and reports stay outside Git.
"""

import argparse
import base64
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path
import time
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
PRIMARY = 'S=2_4x2_T=2=Z=3_CH=2.czi'
# Explicit errors are accepted only where a decoded full-resolution frame
# exceeds the 256 MiB limit (see test-data/czi/provenance.json).
LIMIT_WORDS = ('exceed', 'limit', 'too large', 'unsupported', 'not supported')


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', default='http://127.0.0.1:4446')
    parser.add_argument('--fixtures', type=Path, default=ROOT / 'test-data/czi')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/czi/native-czi.json')
    parser.add_argument('--timeout', type=float, default=90)
    parser.add_argument('--renderer', choices=('gpu', 'cpu'), default='gpu')
    args = parser.parse_args()
    args.output = args.output.absolute()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fixtures = args.fixtures.resolve()
    primary = fixtures / PRIMARY
    expected = json.loads((fixtures / 'expected.json').read_text())['files']
    oversized = {e['file'] for e in json.loads((fixtures / 'provenance.json').read_text())
                 if e.get('summary', {}).get('planeExceeds256MiB')}
    ordinary = ROOT / 'tests/fixtures/rgb-65x49.png'
    sources = sorted(fixtures.glob('*.czi')) + [ordinary]
    hashes = {str(p): digest(p) for p in sources}
    # A damaged copy outside the fixture directory; source files are never written.
    truncated = args.output.parent / 'truncated.czi'
    with primary.open('rb') as source:
        truncated.write_bytes(source.read(64 * 1024))
    scenes = expected[PRIMARY]['scenes']
    sizes = scenes[0]['sizes']
    positions, z_count, time_count, channel_count = len(scenes), sizes.get('Z', 1), sizes.get('T', 1), sizes.get('C', 1)
    assert positions >= 2 and z_count >= 3 and time_count >= 2 and channel_count == 2, sizes

    def app_processes():
        found = set()
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit():
                continue
            try:
                if Path(os.readlink(entry / 'exe')) == args.binary.resolve():
                    found.add(int(entry.name))
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                pass
        return found

    previous_processes = app_processes()

    def request(method, path, body=None):
        req = Request(args.driver_url + path, method=method,
                      data=None if body is None else json.dumps(body).encode(),
                      headers={'Content-Type': 'application/json'})
        with urlopen(req, timeout=max(30, args.timeout)) as response:
            value = json.load(response).get('value')
        if isinstance(value, dict) and value.get('error') and 'message' in value:
            raise RuntimeError(value)
        return value

    sid = request('POST', '/session', {'capabilities': {'alwaysMatch': {
        'browserName': 'wry', 'tauri:options': {'application': str(args.binary.resolve())},
    }}})['sessionId']
    route = f'/session/{sid}'
    report = {'passed': False, 'checks': {}, 'sourceHashes': hashes, 'screenshots': []}
    args.output.with_suffix('.session.json').write_text(json.dumps({'sessionId': sid}) + '\n')

    def js(script, values=None, asynchronous=False):
        return request('POST', route + '/execute/' + ('async' if asynchronous else 'sync'),
                       {'script': script, 'args': values or []})

    def invoke(command, values=None):
        return js('const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(done,e=>done({error:String(e)}));', [command, values or {}], True)

    def state():
        return invoke('image_state')

    def ui():
        return js("""const s=document.getElementById('status'),p=document.getElementById('points-button'),scope=document.getElementById('dimension-scope');
          return {phase:document.getElementById('viewer').dataset.phase,name:document.getElementById('filename').textContent,
            pointsDisabled:p.disabled,pointsTitle:p.title,saveDisabled:document.getElementById('save-button').disabled,
            panelHidden:document.getElementById('navigator-panel').hidden,count:document.getElementById('dimension-count').textContent,
            status:s.textContent,statusError:s.classList.contains('error'),statusVisible:s.getClientRects().length>0&&getComputedStyle(s).visibility!=='hidden',
            scopeHidden:scope.hidden,scope:scope.textContent};""")

    def wait(predicate, description):
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            if result := predicate():
                return result
            time.sleep(.025)
        raise TimeoutError(f'{description}: {state()} {ui()}')

    def click(element):
        js('document.getElementById(arguments[0]).click();', [element])

    def ready(path, position=0, z=0, time_index=0, previous_id=None, mode='none'):
        def current():
            s = state()
            if s and s.get('path') == str(path) and s['id'] != previous_id and s.get('phase') == 'error':
                raise RuntimeError(s.get('error') or s)
            if not s or s.get('path') != str(path) or s.get('phase') != 'ready' or s['id'] == previous_id:
                return False
            if s.get('projection', 'none') != mode:
                return False
            d = s.get('dimensions')
            if d and (d['position'], d['zIndex'], d['timeIndex']) != (position, z, time_index):
                return False
            return s
        result = wait(current, 'ready ' + path.name)
        wait(lambda: ui()['phase'] == 'ready' and ui()['name'] == path.name, 'rendered ready')
        wait(lambda: js("return window.__apertureViewer.world.getItemCount()>0 && Array.from({length:window.__apertureViewer.world.getItemCount()},(_,i)=>window.__apertureViewer.world.getItemAt(i)).every(item=>item.source.getTileUrl(item.source.maxLevel,0,0).includes('/'+arguments[0]+'/')) && window.__apertureViewer.world.getItemAt(0).getTilesToDraw().some(t=>t.tile.loaded);", [result['id']]), 'rendered tiles for current native id')
        return result

    def choose(path):
        js("window.__cziTest.path=arguments[0];document.getElementById('open-button').click();", [str(path)])

    def open_ready(path):
        previous = state()
        choose(path)
        return ready(path, previous_id=previous['id'] if previous else None)

    def open_terminal(path):
        choose(path)
        return wait(lambda: (current := state()) and current.get('path') == str(path) and current['phase'] in ('ready', 'error') and current, 'terminal state ' + path.name)

    def axis(name):
        if ui()['panelHidden']:
            click('navigator-button')
        wait(lambda: js("return !document.getElementById('dimension-axis').disabled;"), 'enabled dimension axis')
        js("const e=document.getElementById('dimension-axis');e.value=arguments[0];e.dispatchEvent(new Event('change',{bubbles:true}));", [name])

    def range_value(value):
        js("const e=document.getElementById('dimension-range');e.value=String(arguments[0]);e.dispatchEvent(new Event('input',{bubbles:true}));", [value])

    def keys(count):
        actions = []
        for _ in range(count):
            actions += [{'type': 'keyDown', 'value': ''}, {'type': 'keyUp', 'value': ''}]
        request('POST', route + '/actions', {'actions': [{'type': 'key', 'id': 'keyboard', 'actions': actions}]})
        request('DELETE', route + '/actions')

    def screenshot(name):
        path = args.output.parent / (name + '.png')
        path.write_bytes(base64.b64decode(request('GET', route + '/screenshot')))
        report['screenshots'].append(str(path))

    def plane_hash(s):
        # Use the native protocol URL exposed by the actual OSD tile source.
        data = js("""
          const done=arguments[arguments.length-1],s=arguments[0];
          const source=window.__apertureViewer.world.getItemAt(0).source;
          const url=source.getTileUrl(source.maxLevel,0,0);
          if(!url.includes('/'+s.id+'/')){done({error:'Displayed source id differs from backend'});return;}
          const base=url.split('/'+s.id+'/')[0]+'/'+s.id;
          Promise.all(s.channels.map((_,c)=>fetch(base+'/channels/'+c+'/preview.png').then(r=>{
            if(!r.ok)throw new Error('Tile '+r.status);return r.arrayBuffer();
          }).then(b=>Array.from(new Uint8Array(b))))).then(done,e=>done({error:String(e)}));
        """, [s], True)
        assert isinstance(data, list), data
        return [hashlib.sha256(bytes(channel)).hexdigest() for channel in data]

    def framebuffer():
        result = js("""
          const done=arguments[arguments.length-1],v=window.__apertureViewer;
          const canvas=v.drawer.canvas,gl=canvas.dataset.colorRenderer==='webgl2'?canvas.getContext('webgl2'):null;
          function capture(){
            try{
              const w=canvas.width,h=canvas.height;
              let pixels;
              if(gl){pixels=new Uint8Array(w*h*4);gl.readPixels(0,0,w,h,gl.RGBA,gl.UNSIGNED_BYTE,pixels);if(gl.getError()!==gl.NO_ERROR)throw new Error('Framebuffer read failed');}
              else pixels=canvas.getContext('2d').getImageData(0,0,w,h).data;
              let hash=2166136261,nonblack=0;const colors=new Set();
              for(let y=0;y<h;y+=3)for(let x=0;x<w;x+=3){
                const at=(y*w+x)*4,r=pixels[at],g=pixels[at+1],b=pixels[at+2];
                hash=Math.imul(hash^r,16777619)>>>0;hash=Math.imul(hash^g,16777619)>>>0;hash=Math.imul(hash^b,16777619)>>>0;
                if(r||g||b){nonblack++;colors.add((r<<16)|(g<<8)|b);}
              }
              done({hash,nonblack,colors:colors.size,width:w,height:h,renderer:v.drawer.getType()});
            }catch(e){done({error:String(e)});}
          }
          if(gl){
            const timer=setTimeout(()=>{canvas.removeEventListener('aperture-color-draw',redrawn);done({error:'Viewport redraw timed out'});},10000);
            function redrawn(){clearTimeout(timer);canvas.removeEventListener('aperture-color-draw',redrawn);capture();}
            canvas.addEventListener('aperture-color-draw',redrawn);v.forceRedraw();
          }else{v.forceRedraw();requestAnimationFrame(()=>requestAnimationFrame(capture));}
        """, asynchronous=True)
        assert result.get('nonblack', 0) > 0 and result.get('colors', 0) > 1, result
        return result

    def read_only_ui():
        current = ui()
        assert current['pointsDisabled'] and current['saveDisabled'], current
        assert 'annotations unavailable' in current['pointsTitle'].lower(), current
        assert 'annotations unavailable' in current['status'].lower() and not current['statusError'], current
        return current

    def explicit_error(path, words):
        failed = open_terminal(path)
        assert failed['phase'] == 'error' and failed.get('error'), failed
        message = failed['error']
        assert any(word in message.lower() for word in words), message
        wait(lambda: (current := ui())['statusError'] and current['statusVisible'] and current['status'] == message and current, 'visible error status ' + path.name)
        return message

    try:
        url = request('GET', route + '/url').split('?')[0]
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1' + ('&colorRenderer=cpu' if args.renderer == 'cpu' else '')})
        wait(lambda: js('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;'), 'app startup')
        js("""
          window.__cziTest={path:null};const original=window.fetch;
          window.fetch=function(url,options){
            if(String(url).includes('plugin%3Adialog%7Copen')){
              window.__cziTest.openOptions=JSON.parse(options.body);
              return Promise.resolve(new Response(JSON.stringify(window.__cziTest.path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            }
            return original.call(this,url,options);
          };
        """)
        first = open_ready(primary)
        d = first['dimensions']
        assert first['readOnly'] and first['channelMode'] and len(first['channels']) == channel_count, first
        assert (first['width'], first['height']) == (sizes['X'], sizes['Y']), (first, sizes)
        assert (len(d['positions']), d['z'], d['time']) == (positions, z_count, time_count), first
        assert 'czi' in invoke('image_extensions')
        assert str(primary) in invoke('folder_images', {'path': str(primary)})
        assert 'czi' in js('return window.__cziTest.openOptions.options.filters[0].extensions;')
        report['checks']['metadataAndReadOnlyUi'] = {'state': first, 'ui': read_only_ui()}
        # A read-only regression may only ever overwrite this disposable copy.
        refused = args.output.parent / ('refused-save' + primary.suffix)
        shutil.copyfile(primary, refused)
        refused_bytes = refused.read_bytes()
        for command, values in [
            ('load_annotations', {'id': first['id']}),
            ('annotation_target', {'id': first['id']}),
            ('save_annotations', {'id': first['id'], 'path': str(refused), 'rois': [], 'overwrite': True}),
        ]:
            result = invoke(command, values)
            message = (result.get('error') or '').lower() if isinstance(result, dict) else ''
            # The disposable path names the format; require the refusal wording itself.
            assert 'read-only' in message, (command, result)
        assert refused.read_bytes() == refused_bytes
        refused.unlink()
        report['checks']['backendRefusesAnnotationAndSave'] = True
        initial_hash = plane_hash(first)
        initial_framebuffer = framebuffer()
        report['checks']['coldCompositeVisiblePixels'] = initial_framebuffer
        click('navigator-button')
        assert not ui()['panelHidden'], ui()
        current = ui()
        assert not current['scopeHidden'] and 'unavailable' in current['scope'].lower(), current
        last_z = z_count - 1
        axis('position')
        click('dimension-next')
        p1 = ready(primary, position=1)
        assert p1['id'] != first['id'] and p1['path'] == first['path'], p1
        axis('z')
        range_value(z_count)
        ready(primary, position=1, z=last_z)
        axis('time')
        js("document.getElementById('dimension-scroll').dispatchEvent(new WheelEvent('wheel',{deltaY:100,bubbles:true,cancelable:true}));")
        ready(primary, position=1, z=last_z, time_index=1)
        axis('z')
        range_value(1)
        ready(primary, position=1, z=0, time_index=1)
        js("document.getElementById('dimension-range').focus();")
        keys(1)
        ready(primary, position=1, z=1, time_index=1)
        assert not ui()['panelHidden'], ui()
        assert js("return document.activeElement.id==='dimension-range';"), 'range lost keyboard focus'
        # Consecutive native key events must retain focus while replies are pending
        # and clamp at the last Z plane.
        keys(z_count)
        ready(primary, position=1, z=last_z, time_index=1)
        assert js("return document.activeElement.id==='dimension-range';"), 'continuous input lost focus'
        range_value(1)
        ready(primary, position=1, z=0, time_index=1)
        # Multiple events in one task exercise coalescing and final-axis clamping.
        js("const e=document.getElementById('dimension-scroll');for(let i=0;i<8;i++)e.dispatchEvent(new WheelEvent('wheel',{deltaY:100,bubbles:true,cancelable:true}));")
        ready(primary, position=1, z=last_z, time_index=1)
        range_value(2)
        moved = ready(primary, position=1, z=1, time_index=1)
        report['checks']['continuousKeysAndCoalescedWheel'] = True
        moved_hash = plane_hash(moved)
        moved_framebuffer = framebuffer()
        assert moved_framebuffer['hash'] != initial_framebuffer['hash'], (initial_framebuffer, moved_framebuffer)
        assert moved_hash != initial_hash, (moved_hash, initial_hash)
        report['checks']['positionZTimeWheelKeyboard'] = {'first': initial_hash, 'selected': moved_hash}
        axis('channel')
        before = state()['id']
        range_value(2)
        assert state()['id'] == before and ui()['count'] == f'2 / {channel_count + 1}', ui()
        range_value(1)
        report['checks']['channelSelectionDoesNotReopen'] = True
        # Revisit an earlier plane through the real frontend loader.
        axis('position')
        range_value(1)
        ready(primary, position=0, z=1, time_index=1)
        axis('z')
        range_value(1)
        ready(primary, time_index=1)
        axis('time')
        range_value(1)
        returned = ready(primary)
        assert plane_hash(returned) == initial_hash
        report['checks']['cacheSeparatesCoordinates'] = True
        # Z maximum projection through the real checkbox, then clear it.
        axis('position')
        before = state()
        click('dimension-max-z')
        projected = ready(primary, previous_id=before['id'], mode='z')
        projected_hash = plane_hash(projected)
        assert projected_hash != initial_hash, (projected_hash, initial_hash)
        framebuffer()
        axis('z')
        assert js("return document.getElementById('dimension-range').disabled && document.getElementById('dimension-count').textContent.startsWith('Max of ');"), ui()
        assert js("return document.getElementById('dimension-max-z').checked && !document.getElementById('dimension-projection-status').hidden;"), ui()
        read_only_ui()
        before = state()
        click('dimension-max-z')
        cleared = ready(primary, previous_id=before['id'])
        assert plane_hash(cleared) == initial_hash
        report['checks']['zMaxProjectionTogglesAndClears'] = {'projected': projected_hash}
        screenshot('czi-navigator')
        # Use the real native window resize API; inspect the control at minimum size.
        rect = request('GET', route + '/window/rect')
        # A tiling compositor overrides client size requests. Float only this
        # harness-owned application's window, leaving other desktop clients alone.
        if os.environ.get('HYPRLAND_INSTANCE_SIGNATURE'):
            clients = json.loads(subprocess.check_output(['hyprctl', '-j', 'clients'], text=True))
            owned = app_processes() - previous_processes
            for client in clients:
                if client.get('pid') not in owned:
                    continue
                selector = json.dumps('address:' + client['address'])
                if not client.get('floating'):
                    floating = subprocess.run(['hyprctl', 'dispatch', 'setfloating', 'address:' + client['address']], capture_output=True, text=True)
                    if floating.returncode:
                        # Hyprland 0.55+ exposes Lua dispatchers.
                        subprocess.run(['hyprctl', 'dispatch', 'hl.dsp.window.float({action="set",window=' + selector + '})'], check=True, capture_output=True)
                subprocess.run(['hyprctl', 'dispatch', 'hl.dsp.window.resize({x=640,y=400,window=' + selector + '})'], check=True, capture_output=True)
        request('POST', route + '/window/rect', {'width': 640, 'height': 400})
        wait(lambda: js('return innerWidth<=660 && innerHeight<=420;'), 'minimum native window size')
        layout = js("const r=document.getElementById('navigator-panel').getBoundingClientRect();return {left:r.left,right:r.right,bottom:r.bottom,width:innerWidth,height:innerHeight,overflow:document.documentElement.scrollWidth>innerWidth};")
        assert layout['left'] >= 0 and layout['right'] <= layout['width'] and layout['bottom'] <= layout['height'] and not layout['overflow'], layout
        screenshot('czi-navigator-minimum')
        report['checks']['minimumNativeLayout'] = layout
        request('POST', route + '/window/rect', {'width': rect['width'], 'height': rect['height']})
        # Queue a real UI selection then replace it through Open before its reply.
        axis('position')
        range_value(positions)
        final = open_ready(ordinary)
        for _ in range(20):
            assert state()['id'] == final['id'] and ui()['name'] == ordinary.name, state()
            time.sleep(.05)
        assert not final.get('readOnly') and not final.get('dimensions'), final
        report['checks']['replacementIgnoresOldPlaneWork'] = True
        open_ready(primary)
        axis('position')
        js("const e=document.getElementById('dimension-range');e.value=String(arguments[0]);e.dispatchEvent(new Event('input',{bubbles:true}));document.getElementById('cancel-button').click();", [positions])
        wait(lambda: ui()['phase'] == 'cancelled', 'UI selected plane cancelled')
        recovered = open_ready(primary)
        assert plane_hash(recovered) == initial_hash
        framebuffer()
        report['checks']['planeCancellationAndReopen'] = True
        # Backend-only contract phase deliberately bypasses the frontend loader.
        # No rendered assertion is made until Open gives the loader a fresh id.
        selected = invoke('select_plane', {'id': recovered['id'], 'position': positions - 1, 'z': last_z, 'time': time_count - 1})
        assert selected['id'] != recovered['id'] and selected['path'] == str(primary), selected
        assert (selected['dimensions']['position'], selected['dimensions']['zIndex'], selected['dimensions']['timeIndex']) == (positions - 1, last_z, time_count - 1), selected
        invalid = invoke('select_plane', {'id': selected['id'], 'position': positions, 'z': 0, 'time': 0})
        assert invalid.get('error'), invalid
        assert state()['id'] == selected['id'], state()
        stale = invoke('select_plane', {'id': first['id'], 'position': 0, 'z': 0, 'time': 0})
        assert stale.get('error'), stale
        report['checks']['immediateCoordinatesAndInvalidSelection'] = True
        open_ready(primary)
        # Damaged and oversized files must fail with a visible explicit message.
        report['checks']['truncatedExplained'] = explicit_error(truncated, ('czi', 'truncated', 'damaged', 'invalid', 'unsupported'))
        for name in sorted(oversized):
            path = fixtures / name
            if path.exists():
                terminal = open_terminal(path)
                if terminal['phase'] == 'ready':
                    s = ready(path)
                    report['checks']['decode-' + name] = {'width': s['width'], 'height': s['height'], 'channels': len(s['channels'])}
                    continue
                report['checks']['oversizedExplained-' + name] = explicit_error(path, LIMIT_WORDS)
        # Every other downloaded fixture must decode and show native pixels.
        for name in sorted(set(expected) - oversized):
            path = fixtures / name
            terminal = open_terminal(path)
            if terminal['phase'] == 'error':
                raise AssertionError(terminal)
            s = ready(path)
            assert s['readOnly'], s
            reference = expected[name]['scenes'][0]['sizes']
            assert (s['width'], s['height']) == (reference['X'], reference['Y']), (name, s, reference)
            framebuffer()
            report['checks']['decode-' + name] = {'width': s['width'], 'height': s['height'], 'channels': len(s['channels']),
                                                  'channelMode': s.get('channelMode'), 'dimensions': s.get('dimensions')}
        for path, value in hashes.items():
            assert digest(Path(path)) == value, path
        report['checks']['sourceSha256Unchanged'] = True
        screenshot('czi-last-fixture')
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
            screenshot('czi-failure')
        except Exception as capture_error:
            report['captureError'] = repr(capture_error)
        raise
    finally:
        args.output.write_text(json.dumps(report, indent=2) + '\n')
        request('DELETE', route)
    print(json.dumps({'passed': report['passed'], 'checks': list(report['checks']), 'report': str(args.output)}, indent=2))


if __name__ == '__main__':
    main()
