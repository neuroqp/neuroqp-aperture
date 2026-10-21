#!/usr/bin/env python3
"""Validate folder navigation in the native Linux app through WebDriver and Rust IPC.

Run inside scripts/headless-session --driver 4446 (use --keep for a cache under
artifacts/folder-navigation). Only the file chooser response is supplied.
Navigation, annotation editing and saving use UI handlers and the production
backend. Native warning dialogs are answered through compositor.py by default;
--bridge-dialogs explicitly reduces dialog coverage.
No Python packages required. Test files and screenshots remain outside Git.
"""

import argparse
import base64
import json
from http.client import RemoteDisconnected
from pathlib import Path
import shutil
import time
from urllib.request import Request, urlopen

import compositor
from compositor import APP_ID


ROOT = Path(__file__).resolve().parents[1]
BIG_TIFF = ROOT / 'test-data/gray16-big-deflate-p2-r64.tif'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', default='http://127.0.0.1:4446')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/folder-navigation/native-navigation.json')
    parser.add_argument('--large-image', type=Path, default=BIG_TIFF)
    parser.add_argument('--channel-image', type=Path, default=ROOT / 'test-data/color-five-channel-imagej.tif')
    parser.add_argument('--timeout', type=float, default=180)
    parser.add_argument('--bridge-dialogs', action='store_true')
    args = parser.parse_args()
    args.output = args.output.absolute()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    desktop = compositor.session()
    binary = compositor.application(args.binary)
    corpus = args.output.parent / 'corpus' / str(time.time_ns())
    corpus.mkdir(parents=True)
    fixtures = ROOT / 'tests/fixtures'
    files = corpus / 'formats'
    files.mkdir()
    names = [
        ('00.png', 'rgb-65x49.png'),
        ('01-' + 'long-microscopy-filename-' * 7 + '.PNG', 'rgb-65x49.png'),
        ('02.jpeg', 'rgb-65x49.jpg'), ('03.jpg', 'rgb-65x49.jpg'),
        ('04.bmp', 'rgb-65x49.bmp'), ('05.tif', 'gray16-stripped.tif'),
        ('06.TIFF', 'gray16-deflate.tif'), ('07.webp', 'rgb-65x49.webp'),
        ('08-corrupt.tif', 'corrupt.tif'), ('09-final.png', 'rgb-65x49.png'),
    ]
    for name, source in names:
        shutil.copyfile(fixtures / source, files / name)
    for name in ('ignore.txt', 'ignore.ome', 'ignore.gif', 'ignore.svg'):
        (files / name).write_text('not an image\n')
    (files / 'directory.png').mkdir()
    (files / 'directory.png' / 'nested.png').write_bytes((fixtures / 'rgb-65x49.png').read_bytes())
    points = corpus / 'points'
    points.mkdir()
    for name in ('00-edited.tif', '01-next.tif'):
        shutil.copyfile(fixtures / 'gray16-stripped.tif', points / name)
    rapid = corpus / 'rapid'
    rapid.mkdir()
    for name in ('00-before.png', '02-middle.png', '03-final.png'):
        shutil.copyfile(fixtures / 'rgb-65x49.png', rapid / name)
    # The large source is read-only throughout these tests; aliases avoid copying 1.5 GB.
    (rapid / '01-large.tif').symlink_to(args.large_image.resolve())
    source_stats = {str(path): (path.stat().st_size, path.stat().st_mtime_ns)
                    for path in (fixtures / source for _, source in names)}
    source_stats[str(args.large_image.resolve())] = (args.large_image.stat().st_size, args.large_image.stat().st_mtime_ns)

    def request(method, path, body=None):
        req = Request(args.driver_url + path,
                      data=None if body is None else json.dumps(body).encode(),
                      method=method, headers={'Content-Type': 'application/json'})
        read_only = method == 'GET' or (path.endswith('/execute/sync') and body['script'].startswith('return ')) or (path.endswith('/execute/async') and body.get('args', [None])[0] == 'image_state')
        for attempt in range(3):
            try:
                with urlopen(req, timeout=30) as response:
                    value = json.load(response).get('value')
                break
            except RemoteDisconnected:
                if not read_only or attempt == 2:
                    raise
                time.sleep(.05)
        if isinstance(value, dict) and isinstance(value.get('error'), str) and 'message' in value:
            raise RuntimeError(value)
        return value

    sid = request('POST', '/session', {'capabilities': {'alwaysMatch': {
        'browserName': 'wry', 'tauri:options': {'application': str(binary)},
    }}})['sessionId']
    route = f'/session/{sid}'
    args.output.with_suffix('.session.json').write_text(json.dumps({'sessionId': sid}) + '\n')
    report = {'passed': False, 'corpus': str(corpus), 'dialogMode': 'bridge' if args.bridge_dialogs else 'native',
              'checks': {}, 'screenshots': []}

    def js(script, values=None, asynchronous=False):
        return request('POST', route + '/execute/' + ('async' if asynchronous else 'sync'),
                       {'script': script, 'args': values or []})

    def invoke(command, values=None):
        return js('const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(done,e=>done({error:String(e)}));', [command, values or {}], True)

    def state():
        return invoke('image_state')

    def ui():
        return js("return {phase:document.getElementById('viewer').dataset.phase,filename:document.getElementById('filename').textContent,dirty:document.getElementById('filename').classList.contains('unsaved'),count:document.getElementById('point-count').textContent,status:document.getElementById('status').textContent,previousDisabled:document.getElementById('previous-image-button').disabled,nextDisabled:document.getElementById('next-image-button').disabled,confirmations:window.__navigationTest.confirmations};")

    def wait(predicate, description, timeout=None):
        deadline = time.monotonic() + (timeout or args.timeout)
        while time.monotonic() < deadline:
            if result := predicate():
                return result
            time.sleep(.025)
        raise TimeoutError(f'{description}: {state()} {ui()}')

    def click(element):
        js('document.getElementById(arguments[0]).click();', [element])

    def open_image(path):
        js("window.__navigationTest.path=arguments[0];document.getElementById('open-button').click();", [str(path.absolute())])

    def ready(path):
        expected = str(path.absolute())
        result = wait(lambda: (s := state()) and s.get('path') == expected and s['phase'] == 'ready' and s,
                      f'ready {path.name}')
        wait(lambda: ui()['phase'] == 'ready' and ui()['filename'] == path.name,
             'rendered ready')
        wait(lambda: js('return window.__apertureViewer.world.getItemCount()>0;'), 'viewer image')
        return result

    def open_ready(path):
        open_image(path)
        return ready(path)

    def resize(width, height):
        windows = [w for w in desktop.windows() if w['app_id'] == APP_ID and w['title'] == 'Aperture']
        assert len(windows) == 1, 'Expected only the owned validation window'
        command = Path(f"/proc/{windows[0]['pid']}/cmdline").read_bytes().split(b'\0')
        assert command[0] == str(binary).encode(), command
        desktop.resize(windows[0], width, height)
        desktop.focus(windows[0])
        actual = wait(lambda: js('return {width:window.innerWidth,height:window.innerHeight};'), 'native window size')
        time.sleep(.4)
        actual = js('return {width:window.innerWidth,height:window.innerHeight};')
        assert actual == {'width':width,'height':height}, (width,height,actual)

    def screenshot(name, whole_screen=False):
        path = args.output.parent / (name + '.png')
        if whole_screen:
            desktop.screenshot(path)
        else:
            path.write_bytes(base64.b64decode(request('GET', route + '/screenshot')))
        report['screenshots'].append(str(path))
        return str(path)

    def workers():
        found = []
        for proc in Path('/proc').iterdir():
            if not proc.name.isdigit():
                continue
            try:
                command = proc.joinpath('cmdline').read_bytes().split(b'\0')
                if command[:2] == [str(args.binary.resolve()).encode(), b'--image-worker']:
                    found.append(int(proc.name))
            except OSError:
                pass
        return found

    def prove_final(path, old_workers):
        final = ready(path)
        wait(lambda: not any(Path(f'/proc/{pid}').exists() for pid in old_workers),
             'replaced workers reaped', 10)
        for _ in range(30):
            current = state()
            assert current['id'] == final['id'] and current['path'] == str(path.absolute()), current
            assert ui()['filename'] == path.name and ui()['phase'] == 'ready', ui()
            assert js('return Array.from({length:window.__apertureViewer.world.getItemCount()},(_,i)=>window.__apertureViewer.world.getItemAt(i).source.getTileUrl(0,0,0)).every(url=>url.includes(arguments[0]));', [final['id']])
            time.sleep(.05)
        return {'id': final['id'], 'reapedWorkers': old_workers, 'staleEventObservationSeconds': 1.5}

    def actions(x, y, alt=False):
        sequences = [{'type': 'pointer', 'id': 'mouse', 'parameters': {'pointerType': 'mouse'}, 'actions': [
            {'type': 'pointerMove', 'origin': 'viewport', 'x': round(x), 'y': round(y), 'duration': 0},
            {'type': 'pointerDown', 'button': 0}, {'type': 'pointerUp', 'button': 0}]}]
        if alt:
            sequences[0]['actions'].insert(0, {'type': 'pause', 'duration': 100})
            sequences.insert(0, {'type': 'key', 'id': 'keyboard', 'actions': [
                {'type': 'keyDown', 'value': '\ue00a'}, {'type': 'pause', 'duration': 100},
                {'type': 'pause', 'duration': 100}, {'type': 'keyUp', 'value': '\ue00a'}]})
        request('POST', route + '/actions', {'actions': sequences})
        request('DELETE', route + '/actions')

    def point_position():
        return js("const v=window.__apertureViewer,r=v.canvas.getBoundingClientRect(),b=v.world.getItemAt(0).getBounds(),p=v.viewport.pixelFromPoint(b.getCenter(),true);return {x:r.left+p.x,y:r.top+p.y};")

    def warning(discard):
        if args.bridge_dialogs:
            js('window.__navigationTest.discard=arguments[0];', [discard])
        click('next-image-button')
        if args.bridge_dialogs:
            return
        def native_dialog():
            return next((w for w in desktop.windows()
                         if w['app_id'] == APP_ID and w['title'] == 'Unsaved annotations'), None)
        dialog = wait(native_dialog, 'native discard warning', 10)
        time.sleep(.4)
        screenshot('warning-discard' if discard else 'warning-cancel', whole_screen=True)
        if discard:
            # Tauri's GTK custom-buttons dialog orders Discard before Cancel.
            desktop.click(dialog['x'] + dialog['width'] / 4, dialog['y'] + dialog['height'] - 24)
        else:
            desktop.focus(dialog)
            desktop.keys('Escape')
        wait(lambda: not native_dialog(), 'warning dismissed', 10)
        time.sleep(.1)

    try:
        url = request('GET', route + '/url').split('?')[0]
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1'})
        wait(lambda: js('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__;'), 'app startup', 20)
        js("""
          window.__navigationTest={confirmations:0,bridge:arguments[0],discard:false};
          const original=window.fetch;
          window.fetch=function(url,options) {
            const test=window.__navigationTest,request=String(url);
            if(request.endsWith('/load_annotations') && test.holdAnnotations) {
              original.call(this,url,options).catch(()=>{});
              return new Promise(resolve=>test.annotationReject=()=>resolve(new Response(JSON.stringify('Image replaced or cancelled.'),{headers:{'Content-Type':'application/json','Tauri-Response':'error'}})));
            }
            if(request.includes('plugin%3Adialog%7Copen')) {
              test.openOptions=JSON.parse(options.body);
              return Promise.resolve(new Response(JSON.stringify(test.path),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            }
            if(request.includes('plugin%3Adialog%7Cmessage')) {
              test.confirmations++;
              if(test.bridge) return Promise.resolve(new Response(JSON.stringify(test.discard ? 'Discard' : 'Cancel'),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
            }
            return original.call(this,url,options);
          };
        """, [args.bridge_dialogs])
        report['renderer'] = js('return window.__apertureViewer.drawer.getType();')
        expected = [str(files / name) for name, _ in names]
        assert invoke('folder_images', {'path': expected[0]}) == expected
        extensions = invoke('image_extensions')
        assert set(extensions) == {'jpg', 'jpeg', 'png', 'bmp', 'tif', 'tiff', 'webp', 'nd2', 'czi'}, extensions
        open_ready(files / names[0][0])
        options = js('return window.__navigationTest.openOptions;')
        assert options['options']['filters'][0]['extensions'] == extensions, options
        assert ui()['previousDisabled'] and not ui()['nextDisabled'], ui()
        before = state()['id']
        click('previous-image-button')
        assert state()['id'] == before
        report['checks']['extensionsChooserAndFirstBoundary'] = {'extensions': extensions, 'paths': expected}
        for button, target in [('next-image-button', files / names[1][0]),
                               ('previous-image-button', files / names[0][0])]:
            position = js('const r=document.getElementById(arguments[0]).getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};', [button])
            actions(**position)
            ready(target)
        report['checks']['pointerClickChevronsInsideDragRegion'] = 'passed'

        layouts = []
        for width in (1200, 640):
            resize(width, 400 if width == 640 else 800)
            for theme in ('light', 'dark'):
                if js('return document.documentElement.dataset.theme;') != theme:
                    click('settings-button')
                    click('settings-appearance-button')
                    js("const s=document.getElementById('theme-select');s.value=arguments[0];s.dispatchEvent(new Event('change',{bubbles:true}));", [theme])
                    click('settings-close')
                positions = []
                for name, _ in names[:2]:
                    open_ready(files / name)
                    time.sleep(.15)
                    positions.append(js("return ['previous-image-button','next-image-button','open-button'].map(id=>{const r=document.getElementById(id).getBoundingClientRect();return {id,x:r.x,y:r.y,width:r.width,height:r.height};});"))
                    screenshot(f'{theme}-{width}-' + ('short' if name == names[0][0] else 'long'))
                for first, second in zip(*positions):
                    assert abs(first['x'] - second['x']) < .5 and abs(first['y'] - second['y']) < .5, positions
                    assert second['width'] > 20 and second['height'] > 20, second
                assert js('return document.documentElement.scrollWidth<=window.innerWidth;')
                layouts.append({'width': width, 'theme': theme, 'positions': positions})
        report['checks']['fixedPositionsBothThemesMinimumWidth'] = layouts
        resize(1200, 800)

        open_ready(files / names[0][0])
        for name, _ in names[1:]:
            click('next-image-button')
            if name == '08-corrupt.tif':
                wait(lambda: (s := state()) and s.get('path') == str(files / name) and s['phase'] == 'error', 'corrupt image error')
                wait(lambda: ui()['phase'] == 'error', 'visible error')
            else:
                ready(files / name)
        assert ui()['nextDisabled'] and not ui()['previousDisabled'], ui()
        before = state()['id']
        click('next-image-button')
        assert state()['id'] == before
        click('previous-image-button')
        wait(lambda: state()['phase'] == 'error', 'previous error')
        click('previous-image-button')
        ready(files / '07.webp')
        report['checks']['allFormatsPreviousNextBoundariesErrorRecovery'] = 'passed'

        for phase in ('before-preview', 'detail'):
            open_ready(rapid / '00-before.png')
            click('next-image-button')
            if phase == 'before-preview':
                loading = wait(lambda: (s := state()) and s.get('path') == str(rapid / '01-large.tif') and s['phase'] == 'loading' and not s['previewReady'] and s, 'large pre-preview')
                old_workers = wait(workers, 'large decode worker', 10)
            else:
                loading = wait(lambda: (s := state()) and s.get('path') == str(rapid / '01-large.tif') and s['phase'] == 'loading' and s['previewReady'] and s, 'large detail phase')
                old_workers = workers()
                assert old_workers, 'No detail worker'
                screenshot('large-detail-navigation')
            js("document.getElementById('next-image-button').click();document.getElementById('next-image-button').click();")
            report['checks']['rapidNavigation-' + phase] = {**prove_final(rapid / '03-final.png', old_workers), 'replacedPhase': loading}
            assert js("return document.getElementById('progress').hidden && document.getElementById('loading-percent').hidden;")

        # Keep the real native annotation call pending at the UI boundary, then
        # deliver its expected cancellation error after Cancel has completed.
        js('window.__navigationTest.holdAnnotations=false;')
        open_ready(rapid / '00-before.png')
        js('window.__navigationTest.annotationReject=null;window.__navigationTest.holdAnnotations=true;')
        click('next-image-button')
        wait(lambda: js('return !!window.__navigationTest.annotationReject;'), 'pending annotations before cancel')
        click('cancel-button')
        wait(lambda: state()['phase'] == 'cancelled' and ui()['phase'] == 'cancelled', 'standalone native cancel')
        js('window.__navigationTest.holdAnnotations=false;window.__navigationTest.annotationReject();')
        time.sleep(.15)
        assert ui()['phase'] == 'cancelled' and ui()['status'] == 'Cancelled', ui()
        assert not js("return document.getElementById('status').classList.contains('error');")
        report['checks']['cancelWithDelayedAnnotationCancellationReply'] = {'passed': True, 'annotationErrorReplyInjected': True}

        # Hold tile completion after real production jobs start, then require the
        # navigation reset to abort them and release OSD's active-job accounting.
        js('''
          const test=window.__navigationTest,v=window.__apertureViewer,add=v.imageLoader.addJob,seen=new WeakSet();
          v.imageLoader.addJob=function(options) {
            const source=options.source;
            if(!seen.has(source)) {
              seen.add(source);const start=source.downloadTileStart;
              source.downloadTileStart=function(job) {
                start.call(this,job);
                if(test.holdDownloads) {
                  test.jobs.push(job);const abort=job.abort;
                  job.abort=()=>{test.aborts++;abort.call(job);};
                  if(job.userData.imageRequest) job.userData.imageRequest.onload=()=>{test.blocked++;};
                }
              };
            }
            return add.call(this,options);
          };
          const fetch=window.fetch;
          window.fetch=function(url,options) {
            if(test.holdDownloads && String(url).includes('/channels/')) {
              test.blocked++;
              return new Promise((resolve,reject)=>options.signal.addEventListener('abort',()=>reject(new DOMException('Aborted','AbortError')),{once:true}));
            }
            return fetch.call(this,url,options);
          };
        ''')
        for kind, source in [('ordinary', fixtures / 'rgb-65x49.png'), ('channel', args.channel_image)]:
            if not source.is_file():
                report['checks']['activeDownloadAbort-' + kind] = {'skipped': 'channel fixture unavailable'}
                continue
            directory = corpus / ('downloads-' + kind)
            directory.mkdir()
            old = directory / ('00-old' + source.suffix)
            shutil.copyfile(source, old)
            replacement = directory / '01-final.png'
            shutil.copyfile(fixtures / 'rgb-65x49.png', replacement)
            js('Object.assign(window.__navigationTest,{holdDownloads:true,jobs:[],aborts:0,blocked:0});')
            open_ready(old)
            wait(lambda: js('return window.__navigationTest.jobs.length>0 && window.__navigationTest.blocked>0;'), 'pending ' + kind + ' tile completion')
            observed = js("const t=window.__navigationTest,before=window.__apertureViewer.imageLoader.jobsInProgress;t.holdDownloads=false;document.getElementById('next-image-button').click();return {before,after:window.__apertureViewer.imageLoader.jobsInProgress,aborts:t.aborts,tracked:t.jobs.length,released:t.jobs.every(j=>!j.jobId),requestsStopped:t.jobs.every(j=>j.userData.controller?.signal.aborted || j.userData.imageRequest?.getAttribute('src')==='')};")
            assert observed['before'] > 0 and observed['after'] == 0, observed
            assert observed['aborts'] == observed['tracked'] and observed['released'] and observed['requestsStopped'], observed
            ready(replacement)
            wait(lambda: js('return window.__apertureViewer.world.getItemAt(0).getTilesToDraw().some(entry=>entry.tile.loaded);'), 'replacement rendered pixels')
            report['checks']['activeDownloadAbort-' + kind] = observed

        edited = points / '00-edited.tif'
        open_ready(edited)
        wait(lambda: js("return !document.querySelector('.point-choice').disabled;"), 'annotations editable')
        click('points-button')
        time.sleep(.25)
        position = point_position()
        actions(**position)
        wait(lambda: ui()['dirty'] and ui()['count'] == '1', 'added point')
        old_id = state()['id']
        warning(False)
        assert state()['id'] == old_id and ui()['dirty'] and ui()['count'] == '1', ui()
        report['checks']['addCancelRetainsAnnotations'] = 'passed'
        warning(True)
        ready(points / '01-next.tif')
        assert not ui()['dirty'] and ui()['count'] == '0', ui()
        report['checks']['addDiscardNavigates'] = 'passed'

        click('previous-image-button')
        ready(edited)
        wait(lambda: js("return !document.querySelector('.point-choice').disabled;"), 'annotations editable')
        if js("return document.getElementById('points-panel').hidden;"):
            click('points-button')
        position = point_position()
        actions(**position)
        wait(lambda: ui()['dirty'] and ui()['count'] == '1', 'point before save')
        click('save-button')
        wait(lambda: js("return document.getElementById('save-dialog').open;"), 'save choice')
        click('save-overwrite-button')
        wait(lambda: not ui()['dirty'] and ui()['status'] == 'Saved', 'save completed')
        confirmations = ui()['confirmations']
        click('next-image-button')
        ready(points / '01-next.tif')
        assert ui()['confirmations'] == confirmations
        click('previous-image-button')
        ready(edited)
        wait(lambda: ui()['count'] == '1', 'saved point reload')
        report['checks']['savedAnnotationsNavigateWithoutWarning'] = 'passed'
        time.sleep(.2)
        position = point_position()
        actions(**position, alt=True)
        wait(lambda: ui()['dirty'] and ui()['count'] == '0', 'deleted final point')
        old_id = state()['id']
        warning(False)
        assert state()['id'] == old_id and ui()['dirty'] and ui()['count'] == '0', ui()
        warning(True)
        ready(points / '01-next.tif')
        click('previous-image-button')
        ready(edited)
        wait(lambda: ui()['count'] == '1', 'discarded deletion reloads saved point')
        report['checks']['deleteCancelRetainsAndDiscardNavigates'] = 'passed'
        screenshot('annotations-final')
        for path, previous in source_stats.items():
            stat = Path(path).stat()
            assert (stat.st_size, stat.st_mtime_ns) == previous, f'Original modified: {path}'
        report['checks']['originalSourcesUnchanged'] = 'passed'
        report['passed'] = True
    except Exception as error:
        report['passed'] = False
        report['error'] = repr(error)
        try:
            report['failureState'] = state()
            report['failureUi'] = ui()
            screenshot('failure')
        except Exception as capture_error:
            report['captureError'] = repr(capture_error)
        raise
    finally:
        args.output.write_text(json.dumps(report, indent=2) + '\n')
        request('DELETE', route)
    print(json.dumps({'passed': report['passed'], 'checks': list(report['checks']), 'report': str(args.output)}, indent=2))


if __name__ == '__main__':
    main()
