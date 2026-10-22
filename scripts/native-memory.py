#!/usr/bin/env python3
"""Check shared image-memory settings and ND2 preload in an owned native session.

Run inside scripts/headless-session --driver 4446; its isolated cache/config
directories persist across the app restarts in this check.
Only the chooser response is substituted. Navigation, settings, pixels and Close
use the application's real handlers. This script does not build or start a VM.
"""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import time
from urllib.request import Request, urlopen

import compositor

ROOT = Path(__file__).resolve().parents[1]
GIB = 1024 ** 3


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            result.update(block)
    return result.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', help='defaults to the session driver; must match it')
    parser.add_argument('--fixtures', type=Path, default=ROOT / 'test-data/nd2')
    parser.add_argument('--real', type=Path, help='Optional larger ND2 acquisition')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/nd2/native-memory.json')
    parser.add_argument('--timeout', type=float, default=120)
    parser.add_argument('--expected-default-gib', type=float, default=4)
    parser.add_argument('--renderer', choices=('gpu', 'cpu'), default='gpu')
    args = parser.parse_args()
    args.driver_url = compositor.driver_url(args.driver_url)
    binary = args.binary.resolve()
    primary = args.fixtures.resolve() / 'dims_p4z5t3c2y32x32.nd2'
    ordinary = [ROOT / f'tests/fixtures/rgb-65x49.{extension}'
                for extension in ('png', 'jpg', 'webp', 'bmp')]
    ordinary.append(ROOT / 'tests/fixtures/rgba-alpha.png')
    sources = [primary, *ordinary] + ([args.real.resolve()] if args.real else [])
    hashes = {str(path): digest(path) for path in sources}
    output = args.output.absolute()
    output.parent.mkdir(parents=True, exist_ok=True)
    report = {'passed': False, 'checks': {}, 'sourceHashes': hashes,
              'memorySamples': [], 'preloadSamples': [], 'screenshots': [],
              'memoryMeaning': 'Explicit image leases across processes, not total RSS.'}
    route = None

    def request(method, path, body=None):
        req = Request(args.driver_url + path, method=method,
                      data=None if body is None else json.dumps(body).encode(),
                      headers={'Content-Type': 'application/json'})
        with urlopen(req, timeout=max(30, args.timeout)) as response:
            value = json.load(response).get('value')
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

    def wait(predicate, description):
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            result = predicate()
            if result:
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

    def start():
        nonlocal route
        previous = processes()
        sid = request('POST', '/session', {'capabilities': {'alwaysMatch': {
            'browserName': 'wry', 'tauri:options': {'application': str(compositor.application(binary))},
        }}})['sessionId']
        route = f'/session/{sid}'
        output.with_suffix('.session.json').write_text(json.dumps({'sessionId': sid}) + '\n')
        url = request('GET', route + '/url').split('?')[0]
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1' + ('&colorRenderer=cpu' if args.renderer == 'cpu' else '')})
        wait(lambda: js('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;'), 'startup')
        js("""
          window.__memoryTest={path:null};const original=window.fetch;
          window.fetch=function(url,options){
            if(String(url).includes('plugin%3Adialog%7Copen'))
              return Promise.resolve(new Response(JSON.stringify(window.__memoryTest.path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            return original.call(this,url,options);
          };
        """)
        return previous

    def close(previous):
        nonlocal route
        owned = processes() - previous
        assert owned, 'owned native process missing before Close'
        js("setTimeout(()=>document.getElementById('close-button').click(),100);return true;")
        wait(lambda: not processes() & owned, 'normal Close reaps native processes')
        request('DELETE', route)
        route = None

    def memory():
        value = invoke('memory_status')
        assert isinstance(value, dict) and 'limitBytes' in value and 'usedBytes' in value, value
        assert 0 <= value['usedBytes'] <= value['limitBytes'], value
        report['memorySamples'].append(value)
        return value

    def preload():
        value = invoke('nd2_preload_status')
        assert isinstance(value, dict) and all(key in value for key in
            ('cachedFrames', 'retainedBytes', 'previewsReady', 'framesReady', 'running')), value
        assert value['retainedBytes'] >= 0, value
        report['preloadSamples'].append(value)
        memory()
        return value

    def ready(path, position=0, z=0, time_index=0, previous_id=None):
        def selected():
            value = state()
            if value and value.get('path') == str(path) and value['id'] != previous_id and value.get('phase') == 'error':
                raise RuntimeError(value.get('error') or value)
            if not value or value.get('path') != str(path) or value.get('phase') != 'ready' or value['id'] == previous_id:
                return False
            d = value.get('dimensions')
            if d is None:
                return value
            return value if (d['position'], d['zIndex'], d['timeIndex']) == (position, z, time_index) else False
        result = wait(selected, 'ready ' + path.name)
        wait(lambda: js("""
          const v=window.__apertureViewer;
          return document.getElementById('viewer').dataset.phase==='ready' &&
            v.world.getItemCount()>0 && Array.from({length:v.world.getItemCount()},(_,i)=>v.world.getItemAt(i)).every(item=>
              item.source.getTileUrl(item.source.maxLevel,0,0).includes('/'+arguments[0]+'/')) &&
            v.world.getItemAt(0).getTilesToDraw().some(t=>t.tile.loaded);
        """, [result['id']]), 'visible current plane')
        memory()
        return result

    def open_ready(path):
        old = state()
        js("window.__memoryTest.path=arguments[0];document.getElementById('open-button').click();", [str(path)])
        return ready(path, previous_id=old['id'] if old else None)

    def axis(name):
        js("if(document.getElementById('navigator-panel').hidden)document.getElementById('navigator-button').click();const e=document.getElementById('dimension-axis');e.value=arguments[0];e.dispatchEvent(new Event('change',{bubbles:true}));", [name])

    def range_value(value):
        js("const e=document.getElementById('dimension-range');e.value=String(arguments[0]);e.dispatchEvent(new Event('input',{bubbles:true}));", [value])

    def plane_hash(value):
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
        return [hashlib.sha256(bytes(channel)).hexdigest() for channel in data]

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

    def screenshot(name):
        path = output.parent / (name + '.png')
        path.write_bytes(base64.b64decode(request('GET', route + '/screenshot')))
        report['screenshots'].append(str(path))

    original_limit = None
    try:
        previous = start()
        original_limit = memory()['limitBytes']
        assert original_limit == round(args.expected_default_gib * GIB), memory()
        report['checks']['defaultBudget'] = original_limit
        first = open_ready(primary)
        assert first['sampleBits'] == 16 and first['channelMode'], first
        first_hash, pixels = plane_hash(first), framebuffer()
        click('settings-button')
        wait(lambda: js("return document.getElementById('settings-dialog').open && Number(document.getElementById('memory-limit').value)===arguments[0];", [args.expected_default_gib]), 'memory settings value')
        js("const e=document.getElementById('memory-limit');e.value='0.25';e.dispatchEvent(new Event('input',{bubbles:true}));document.getElementById('memory-apply').click();")
        wait(lambda: invoke('memory_status').get('limitBytes') == GIB // 4 and js("return !document.getElementById('memory-apply').disabled && document.getElementById('memory-error').hidden;"), 'apply quarter GiB')
        screenshot('memory-settings')
        click('settings-close')
        assert state()['id'] == first['id'] and plane_hash(first) == first_hash
        assert framebuffer() == pixels, (pixels, framebuffer())
        report['checks']['settingsPreserveRenderedPixels'] = pixels
        settled = wait(lambda: (p := preload()) and not p['running'] and p['cachedFrames'] > 1 and p['previewsReady'] > 0 and p['framesReady'] > 0 and p, 'completed background preload')
        assert state()['id'] == first['id'] and plane_hash(first) == first_hash
        report['checks']['preloadLeavesCurrentPlaneSelected'] = settled
        axis('z')
        range_value(2)
        selected = ready(primary, z=1)
        selected_hash = plane_hash(selected)
        assert selected_hash != first_hash, (selected_hash, first_hash)
        framebuffer()
        range_value(1)
        returned = ready(primary)
        assert plane_hash(returned) == first_hash
        range_value(2)
        revisited = ready(primary, z=1)
        assert plane_hash(revisited) == selected_hash
        report['checks']['preloadedNavigationHashes'] = {'first': first_hash, 'selected': selected_hash}
        # Sample actual visible OSD items while multiple UI requests coalesce.
        continuity = js("""
          const done=arguments[arguments.length-1],v=window.__apertureViewer;
          let samples=0,blank=0;const deadline=performance.now()+750;
          function sample(){samples++;if(!Array.from({length:v.world.getItemCount()},(_,i)=>v.world.getItemAt(i)).some(item=>item.getOpacity()>0 && item.getTilesToDraw().some(t=>t.tile.loaded)))blank++;
            if(performance.now()<deadline)requestAnimationFrame(sample);else done({samples,blank});}
          const e=document.getElementById('dimension-range');for(const value of [3,4,5,2,1]){e.value=String(value);e.dispatchEvent(new Event('input',{bubbles:true}));}sample();
        """, asynchronous=True)
        assert continuity['samples'] > 1 and continuity['blank'] == 0, continuity
        ready(primary)
        assert plane_hash(state()) == first_hash
        report['checks']['rapidNavigationRetainsVisiblePreview'] = continuity
        preserved = state()
        preserved_pixels = framebuffer()
        # Reject actual channel requests for the new id, retaining the chooser shim.
        js(r"""
          const oldId=arguments[0],original=window.fetch;
          window.__memoryTest.beforeFailureFetch=original;
          window.__memoryTest.failedPreviewRequests=[];
          window.fetch=function(input,options){
            const url=String(typeof input==='string'?input:input.url ?? input);
            if(/\/channels\/\d+\/preview\.(?:png|u16)(?:[?#]|$)/.test(url) && !url.includes('/'+oldId+'/')){
              window.__memoryTest.failedPreviewRequests.push(url);
              return Promise.reject(new Error('Injected native memory test preview failure'));
            }
            return original.call(this,input,options);
          };
        """, [preserved['id']])
        try:
            failed_continuity = js("""
              const done=arguments[arguments.length-1],oldId=arguments[0],v=window.__apertureViewer;
              let samples=0,missingOld=0;const deadline=performance.now()+750;
              function sample(){samples++;
                if(!Array.from({length:v.world.getItemCount()},(_,i)=>v.world.getItemAt(i)).some(item=>
                  item.getOpacity()>0 && item.source.getTileUrl(item.source.maxLevel,0,0).includes('/'+oldId+'/') && item.getTilesToDraw().some(t=>t.tile.loaded)))missingOld++;
                if(performance.now()<deadline)requestAnimationFrame(sample);else done({samples,missingOld});}
              const e=document.getElementById('dimension-range');e.value='2';e.dispatchEvent(new Event('input',{bubbles:true}));sample();
            """, [preserved['id']], True)
            assert failed_continuity['samples'] > 1 and failed_continuity['missingOld'] == 0, failed_continuity
            failure = wait(lambda: js("""
              const e=document.getElementById('status'),requests=window.__memoryTest.failedPreviewRequests;
              return requests.length>0 && e.classList.contains('error') && e.textContent.includes('preview') && {requests,status:e.textContent};
            """), 'production UI reports injected preview failure')
            assert state()['id'] != preserved['id'], state()
            failed_pixels = framebuffer()
            assert failed_pixels == preserved_pixels, (preserved_pixels, failed_pixels)
            screenshot('memory-failed-preview-retained')
            report['checks']['failedNewPreviewRetainsVisibleOldPlane'] = {
                'continuity': failed_continuity, 'failure': failure, 'pixels': failed_pixels,
            }
        finally:
            js('window.fetch=window.__memoryTest.beforeFailureFetch;delete window.__memoryTest.beforeFailureFetch;')
        recovered = open_ready(primary)
        assert plane_hash(recovered) == first_hash
        framebuffer()
        report['checks']['replacementRecoversAfterPreviewFailure'] = True
        # Exercise both Linux CLI decoders and the isolated BMP worker at the
        # minimum budget, through actual Open and visible native pixels.
        ordinary_results = []
        for path in ordinary:
            image = open_ready(path)
            assert not image.get('readOnly') and not image.get('dimensions'), image
            assert (image['width'], image['height']) == (65, 49), image
            ordinary_results.append({'path': str(path), 'pixels': framebuffer(),
                                     'memory': memory()})
        report['checks']['ordinaryFormatsAtMinimumBudget'] = ordinary_results
        open_ready(primary)
        if args.real:
            restored = invoke('set_memory_budget', {'limitBytes': original_limit})
            assert restored.get('limitBytes') == original_limit, restored
            real = open_ready(args.real.resolve())
            report['checks']['realAcquisition'] = {'path': real['path'], 'width': real['width'], 'height': real['height'], 'dimensions': real['dimensions'], 'pixels': framebuffer(), 'memory': memory(), 'preload': preload()}
            # Release the larger acquisition before shrinking the budget again.
            open_ready(primary)
            reduced = invoke('set_memory_budget', {'limitBytes': GIB // 4})
            assert reduced.get('limitBytes') == GIB // 4, reduced
            wait(lambda: invoke('memory_status').get('usedBytes', GIB) <= GIB // 4, 'settled quarter GiB budget')
        memory()
        close(previous)
        report['checks']['normalCloseReapsNativeProcesses'] = True
        previous = start()
        assert memory()['limitBytes'] == GIB // 4, memory()
        click('settings-button')
        wait(lambda: js("return Number(document.getElementById('memory-limit').value)===0.25;"), 'persisted UI budget')
        click('settings-close')
        report['checks']['budgetPersistsAcrossNativeReopen'] = True
        restored = invoke('set_memory_budget', {'limitBytes': original_limit})
        assert restored.get('limitBytes') == original_limit, restored
        for path, source_hash in hashes.items():
            assert digest(Path(path)) == source_hash, path
        report['checks']['sourceSha256Unchanged'] = True
        close(previous)
        report['passed'] = True
    except Exception as error:
        report['error'] = repr(error)
        if route:
            try:
                report['failureState'] = state()
                report['failureMemory'] = invoke('memory_status')
                report['failurePreload'] = invoke('nd2_preload_status')
                screenshot('memory-failure')
            except Exception as capture_error:
                report['captureError'] = repr(capture_error)
        raise
    finally:
        if route:
            try:
                if original_limit is not None:
                    invoke('set_memory_budget', {'limitBytes': original_limit})
                request('DELETE', route)
            except Exception as cleanup_error:
                report['cleanupError'] = repr(cleanup_error)
        output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': report['passed'], 'checks': list(report['checks']), 'report': str(output)}, indent=2))


if __name__ == '__main__':
    main()
