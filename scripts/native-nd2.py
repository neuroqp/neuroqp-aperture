#!/usr/bin/env python3
"""Validate ND2 in an owned native Aperture session using stdlib WebDriver.

Parent orchestration starts tauri-driver. Only the chooser result is supplied;
real UI handlers, Rust IPC, ND2 reader, cache and tile protocol handle the data.
No Python or Java ND2 decoder is used. Fixtures and reports stay outside Git.
"""

import argparse
import base64
import hashlib
import json
import os
import subprocess
from pathlib import Path
import time
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', default='http://127.0.0.1:4446')
    parser.add_argument('--fixtures', type=Path, default=ROOT / 'test-data/nd2')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/nd2/native-nd2.json')
    parser.add_argument('--timeout', type=float, default=90)
    parser.add_argument('--renderer', choices=('gpu', 'cpu'), default='gpu')
    args = parser.parse_args()
    args.output = args.output.absolute()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fixtures = args.fixtures.resolve()
    primary = fixtures / 'dims_p4z5t3c2y32x32.nd2'
    sources = list(fixtures.glob('*.nd2')) + [ROOT / 'tests/fixtures/rgb-65x49.png']
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in sources}
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
        return js("return {phase:document.getElementById('viewer').dataset.phase,name:document.getElementById('filename').textContent,pointsDisabled:document.getElementById('points-button').disabled,saveDisabled:document.getElementById('save-button').disabled,panelHidden:document.getElementById('navigator-panel').hidden,count:document.getElementById('dimension-count').textContent,status:document.getElementById('status').textContent};")

    def wait(predicate, description):
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            if result := predicate():
                return result
            time.sleep(.025)
        raise TimeoutError(f'{description}: {state()} {ui()}')

    def click(element):
        js('document.getElementById(arguments[0]).click();', [element])

    def ready(path, position=0, z=0, time_index=0, previous_id=None):
        def current():
            s = state()
            if not s or s.get('path') != str(path) or s.get('phase') != 'ready' or s['id'] == previous_id:
                return False
            d = s.get('dimensions')
            if d and (d['position'], d['zIndex'], d['timeIndex']) != (position, z, time_index):
                return False
            return s
        result = wait(current, 'ready ' + path.name)
        wait(lambda: ui()['phase'] == 'ready' and ui()['name'] == path.name, 'rendered ready')
        wait(lambda: js("return window.__apertureViewer.world.getItemCount()>0 && Array.from({length:window.__apertureViewer.world.getItemCount()},(_,i)=>window.__apertureViewer.world.getItemAt(i)).every(item=>item.source.getTileUrl(item.source.maxLevel,0,0).includes('/'+arguments[0]+'/')) && window.__apertureViewer.world.getItemAt(0).getTilesToDraw().some(t=>t.tile.loaded);", [result['id']]), 'rendered tiles for current native id')
        return result

    def open_ready(path):
        previous = state()
        js("window.__nd2Test.path=arguments[0];document.getElementById('open-button').click();", [str(path)])
        return ready(path, previous_id=previous['id'] if previous else None)

    def axis(name):
        if ui()['panelHidden']:
            click('navigator-button')
        wait(lambda: js("return !document.getElementById('dimension-axis').disabled;"), 'enabled dimension axis')
        js("const e=document.getElementById('dimension-axis');e.value=arguments[0];e.dispatchEvent(new Event('change',{bubbles:true}));", [name])

    def range_value(value):
        js("const e=document.getElementById('dimension-range');e.value=String(arguments[0]);e.dispatchEvent(new Event('input',{bubbles:true}));", [value])

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

    try:
        url = request('GET', route + '/url').split('?')[0]
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1' + ('&colorRenderer=cpu' if args.renderer == 'cpu' else '')})
        wait(lambda: js('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;'), 'app startup')
        js("""
          window.__nd2Test={path:null};const original=window.fetch;
          window.fetch=function(url,options){
            if(String(url).includes('plugin%3Adialog%7Copen')){
              window.__nd2Test.openOptions=JSON.parse(options.body);
              return Promise.resolve(new Response(JSON.stringify(window.__nd2Test.path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            }
            return original.call(this,url,options);
          };
        """)
        first = open_ready(primary)
        d = first['dimensions']
        assert first['readOnly'] and first['channelMode'] and len(first['channels']) == 2, first
        assert (first['width'], first['height'], len(d['positions']), d['z'], d['time']) == (32, 32, 4, 5, 3), first
        assert ui()['pointsDisabled'] and ui()['saveDisabled'], ui()
        assert 'nd2' in invoke('image_extensions')
        assert str(primary) in invoke('folder_images', {'path': str(primary)})
        assert 'nd2' in js('return window.__nd2Test.openOptions.options.filters[0].extensions;')
        report['checks']['metadataAndReadOnlyUi'] = first
        for command, values in [
            ('load_annotations', {'id': first['id']}),
            ('annotation_target', {'id': first['id']}),
            ('save_annotations', {'id': first['id'], 'path': str(primary), 'rois': [], 'overwrite': True}),
        ]:
            result = invoke(command, values)
            assert result.get('error') and ('ND2' in result['error'] or 'read-only' in result['error'].lower()), (command, result)
        report['checks']['backendRefusesAnnotationAndSave'] = True
        initial_hash = plane_hash(first)
        initial_framebuffer = framebuffer()
        report['checks']['coldCompositeVisiblePixels'] = initial_framebuffer
        click('navigator-button')
        assert not ui()['panelHidden'], ui()
        axis('position')
        click('dimension-next')
        p1 = ready(primary, position=1)
        assert p1['id'] != first['id'] and p1['path'] == first['path'], p1
        axis('z')
        range_value(5)
        ready(primary, position=1, z=4)
        axis('time')
        js("document.getElementById('dimension-scroll').dispatchEvent(new WheelEvent('wheel',{deltaY:100,bubbles:true,cancelable:true}));")
        ready(primary, position=1, z=4, time_index=1)
        axis('z')
        range_value(1)
        ready(primary, position=1, z=0, time_index=1)
        js("document.getElementById('dimension-range').focus();")
        request('POST', route + '/actions', {'actions': [{'type': 'key', 'id': 'keyboard', 'actions': [
            {'type': 'keyDown', 'value': '\ue014'}, {'type': 'keyUp', 'value': '\ue014'},
        ]}]})
        request('DELETE', route + '/actions')
        moved = ready(primary, position=1, z=1, time_index=1)
        assert not ui()['panelHidden'], ui()
        assert js("return document.activeElement.id==='dimension-range';"), 'range lost keyboard focus'
        # Consecutive native key events must retain focus while replies are pending.
        request('POST', route + '/actions', {'actions': [{'type': 'key', 'id': 'keyboard', 'actions': [
            {'type': 'keyDown', 'value': '\ue014'}, {'type': 'keyUp', 'value': '\ue014'},
            {'type': 'keyDown', 'value': '\ue014'}, {'type': 'keyUp', 'value': '\ue014'},
        ]}]})
        request('DELETE', route + '/actions')
        ready(primary, position=1, z=3, time_index=1)
        assert js("return document.activeElement.id==='dimension-range';"), 'continuous input lost focus'
        # Multiple events in one task exercise coalescing and final-axis clamping.
        js("const e=document.getElementById('dimension-scroll');for(let i=0;i<8;i++)e.dispatchEvent(new WheelEvent('wheel',{deltaY:100,bubbles:true,cancelable:true}));")
        ready(primary, position=1, z=4, time_index=1)
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
        assert state()['id'] == before and ui()['count'] == '2 / 3', ui()
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
        screenshot('nd2-navigator')
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
        screenshot('nd2-navigator-minimum')
        report['checks']['minimumNativeLayout'] = layout
        request('POST', route + '/window/rect', {'width': rect['width'], 'height': rect['height']})
        # Queue a real UI selection then replace it through Open before its reply.
        axis('position')
        range_value(4)
        ordinary = ROOT / 'tests/fixtures/rgb-65x49.png'
        final = open_ready(ordinary)
        for _ in range(20):
            assert state()['id'] == final['id'] and ui()['name'] == ordinary.name, state()
            time.sleep(.05)
        assert not final.get('readOnly') and not final.get('dimensions'), final
        report['checks']['replacementIgnoresOldPlaneWork'] = True
        open_ready(primary)
        axis('position')
        js("const e=document.getElementById('dimension-range');e.value='4';e.dispatchEvent(new Event('input',{bubbles:true}));document.getElementById('cancel-button').click();")
        wait(lambda: ui()['phase'] == 'cancelled', 'UI selected plane cancelled')
        recovered = open_ready(primary)
        assert plane_hash(recovered) == initial_hash
        framebuffer()
        report['checks']['planeCancellationAndReopen'] = True
        # Backend-only contract phase deliberately bypasses the frontend loader.
        # No rendered assertion is made until Open gives the loader a fresh id.
        selected = invoke('select_plane', {'id': recovered['id'], 'position': 3, 'z': 4, 'time': 2})
        assert selected['id'] != recovered['id'] and selected['path'] == str(primary), selected
        assert (selected['dimensions']['position'], selected['dimensions']['zIndex'], selected['dimensions']['timeIndex']) == (3, 4, 2), selected
        invalid = invoke('select_plane', {'id': selected['id'], 'position': 4, 'z': 0, 'time': 0})
        assert invalid.get('error'), invalid
        assert state()['id'] == selected['id'], state()
        stale = invoke('select_plane', {'id': first['id'], 'position': 0, 'z': 0, 'time': 0})
        assert stale.get('error'), stale
        report['checks']['immediateCoordinatesAndInvalidSelection'] = True
        open_ready(primary)
        unsupported = fixtures / 'dims_rgb_t3p2c2z3x64y64.nd2'
        if unsupported.exists():
            js("window.__nd2Test.path=arguments[0];document.getElementById('open-button').click();", [str(unsupported)])
            failed = wait(lambda: (s := state()) and s.get('path') == str(unsupported) and s['phase'] == 'error' and s, 'unsupported mixed RGB error')
            assert 'unsupported' in failed['error'].lower(), failed
            report['checks']['unsupportedMixedRgbExplained'] = failed['error']
        # Every downloaded supported dialect must decode and show native pixels.
        for name in ('dims_t3c2y32x32.nd2', 'compressed_lossless.nd2', '1.2audrosophila.nd2',
                     'rgb.nd2',
                     'dims_p4z5t3c2y32x32-tile2x3.nd2',
                     'aryeh_but3_cont200-1.nd2'):
            path = fixtures / name
            if path.exists():
                js("window.__nd2Test.path=arguments[0];document.getElementById('open-button').click();", [str(path)])
                terminal = wait(lambda: (current := state()) and current.get('path') == str(path) and current['phase'] in ('ready', 'error') and current, 'fixture decode terminal state')
                if terminal['phase'] == 'error':
                    message = terminal.get('error', '')
                    if name.startswith('aryeh_') and 'legacy' in message.lower() and any(word in message.lower() for word in ('unsupported', 'not supported')):
                        report.setdefault('gaps', {})['legacyJpeg2000'] = message
                        continue
                    raise AssertionError(terminal)
                s = ready(path)
                assert s['readOnly'] and s['channelMode'], s
                report['checks']['decode-' + name] = {'width': s['width'], 'height': s['height'], 'channels': len(s['channels']), 'dimensions': s['dimensions']}
        for path, digest in hashes.items():
            assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest, path
        report['checks']['sourceSha256Unchanged'] = True
        screenshot('nd2-last-fixture')
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
            screenshot('nd2-failure')
        except Exception as capture_error:
            report['captureError'] = repr(capture_error)
        raise
    finally:
        args.output.write_text(json.dumps(report, indent=2) + '\n')
        request('DELETE', route)
    print(json.dumps({'passed': report['passed'], 'checks': list(report['checks']), 'report': str(args.output)}, indent=2))


if __name__ == '__main__':
    main()
