#!/usr/bin/env python3
"""Validate Settings in a native app launched by an already running tauri-driver.

Run the driver with isolated XDG_CONFIG_HOME/XDG_CACHE_HOME and an owned TMPDIR.
Only the file chooser, confirmation and updater check/download responses are supplied. Image
loading, pointer annotation, theme rendering, modal focus and keyboard input use
the running app. This script neither builds nor installs an update.
"""

import argparse
import base64
import json
from pathlib import Path
import shutil
import subprocess
import time
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', default='http://127.0.0.1:4444')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/settings/result.json')
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    source = ROOT / 'tests/fixtures/gray16-stripped.tif'
    original = source.read_bytes()
    owned = args.output.parent / f'owned-{time.time_ns()}'
    owned.mkdir()
    editable = owned / source.name
    shutil.copyfile(source, editable)
    report = {'passed': False, 'checks': {}, 'screenshots': [],
              'mocked': ['file chooser', 'confirmation response', 'update metadata', 'update download']}

    def request(method, suffix, body=None):
        payload = None if body is None else json.dumps(body).encode()
        with urlopen(Request(args.driver_url + suffix, data=payload, method=method,
                             headers={'Content-Type': 'application/json'}), timeout=35) as response:
            value = json.load(response)['value']
        if isinstance(value, dict) and value.get('error'):
            raise RuntimeError(value)
        return value

    # Capture existing windows before starting the validation app.
    previous_windows = None
    if shutil.which('hyprctl'):
        try:
            previous_windows = json.loads(subprocess.check_output(['hyprctl', '-j', 'clients']))
        except (OSError, subprocess.CalledProcessError, json.JSONDecodeError):
            pass

    session = request('POST', '/session', {'capabilities': {'alwaysMatch': {
        'browserName': 'wry', 'tauri:options': {'application': str(args.binary.resolve())}}}})
    route = '/session/' + session['sessionId']
    args.output.with_suffix('.session.json').write_text(json.dumps(session) + '\n')
    saved_theme = None
    theme_read = False

    def js(code, values=None):
        return request('POST', route + '/execute/sync', {'script': code, 'args': values or []})

    def wait(code, timeout=25):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if result := js(code):
                return result
            time.sleep(.1)
        raise TimeoutError(code)

    def click(element):
        js('document.getElementById(arguments[0]).click()', [element])

    def keys(*values):
        events = []
        for value in values:
            events.extend([{'type': 'keyDown', 'value': value}, {'type': 'keyUp', 'value': value}])
        request('POST', route + '/actions', {'actions': [{'type': 'key', 'id': 'keyboard', 'actions': events}]})
        request('DELETE', route + '/actions')

    def open_settings():
        click('settings-button')
        wait("return document.getElementById('settings-dialog').open")
        assert js("return !document.getElementById('settings-general').hidden && document.getElementById('settings-appearance').hidden && document.getElementById('settings-general-button').getAttribute('aria-current')==='page'"), 'Settings did not open at General'

    def escape():
        keys('\ue00c')
        wait("return !document.getElementById('settings-dialog').open && document.activeElement.id==='settings-button'")

    def set_theme(value):
        js("const s=document.getElementById('theme-select');s.value=arguments[0];s.dispatchEvent(new Event('change',{bubbles:true}));", [value])

    def theme_matches(value):
        return js("return document.getElementById('theme-select').value===arguments[0] && document.documentElement.dataset.theme===(arguments[0]==='system' ? (matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light') : arguments[0]);", [value])

    def snapshot():
        return js("""const v=window.__apertureViewer,b=v.viewport.getBounds(true),i=v.world.getItemAt(0);
          return {phase:document.getElementById('viewer').dataset.phase,
            filename:document.getElementById('filename').textContent,
            dirty:document.getElementById('filename').classList.contains('unsaved'),
            count:document.getElementById('point-count').textContent,
            points:document.querySelector('[data-annotations]').innerHTML,
            bounds:[b.x,b.y,b.width,b.height],zoom:v.viewport.getZoom(true),
            items:v.world.getItemCount(),source:i.source.getTileUrl(0,0,0),
            opens:window.__settingsTest.opens};""")

    def resize(width, height):
        request('POST', route + '/window/rect', {'width': width, 'height': height})
        condition = f'return innerWidth==={width} && innerHeight==={height}'
        try:
            wait(condition, timeout=2)
            return
        except TimeoutError:
            pass
        assert previous_windows is not None, 'Window manager ignored resize and no Hyprland ownership snapshot is available'
        existing_pids = {w['pid'] for w in previous_windows}
        existing_addresses = {w['address'] for w in previous_windows}
        candidates = []
        for window in json.loads(subprocess.check_output(['hyprctl', '-j', 'clients'])):
            if window['pid'] in existing_pids or window['address'] in existing_addresses:
                continue
            try:
                command = Path(f"/proc/{window['pid']}/cmdline").read_bytes().split(b'\0')
            except OSError:
                continue
            if command[0] == str(args.binary.resolve()).encode():
                candidates.append(window)
        assert len(candidates) == 1, f'Expected exactly one new validation window: {candidates}'
        window = candidates[0]
        script = (
            'local owned=nil;for _,w in ipairs(hl.get_windows({class="aperture"})) do '
            f"if w.pid=={window['pid']} and w.address=={json.dumps(window['address'])} then owned=w end end;"
            'assert(owned,"Owned validation window no longer exists");'
            'hl.dispatch(hl.dsp.window.float({action="set",window=owned}));'
            f'hl.dispatch(hl.dsp.window.resize({{x={width},y={height},relative=false,window=owned}}))'
        )
        subprocess.run(['hyprctl', 'eval', script], check=True, stdout=subprocess.DEVNULL)
        wait(condition)
        report['checks']['nativeResizeOwnership'] = {'pid': window['pid'], 'address': window['address']}

    def screenshot(name):
        path = args.output.parent / f'{name}.png'
        path.write_bytes(base64.b64decode(request('GET', route + '/screenshot')))
        report['screenshots'].append(str(path))

    try:
        wait('return !!window.__TAURI_INTERNALS__ && !!document.getElementById("settings-button")')
        saved_theme = js('return localStorage.getItem("aperture-theme")')
        theme_read = True
        url = request('GET', route + '/url').split('?')[0] + '?colorBenchmark=1'
        js('localStorage.removeItem("aperture-theme")')
        request('POST', route + '/url', {'url': url})
        wait('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__')
        assert theme_matches('system'), 'A missing preference must follow system appearance'
        report['checks']['defaultSystemAppearance'] = True
        for preference in ('light', 'dark', 'system'):
            open_settings()
            click('settings-appearance-button')
            set_theme(preference)
            assert theme_matches(preference), f'{preference} did not apply immediately'
            assert js('return localStorage.getItem("aperture-theme")', []) == preference
            escape()
            open_settings()
            click('settings-appearance-button')
            assert theme_matches(preference), f'{preference} was lost on reopen'
            escape()
            request('POST', route + '/refresh', {})
            wait('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__')
            assert theme_matches(preference), f'{preference} was lost on reload'
        report['checks']['themesImmediateReopenReload'] = True

        js('''const original=window.fetch;window.__settingsTest={opens:0,checks:0,downloads:0,confirmations:0,failCheck:false};
          window.fetch=function(url,options){const path=decodeURIComponent(String(url)),t=window.__settingsTest;
            const response=(value,error=false)=>new Response(JSON.stringify(value),{headers:{'Content-Type':'application/json','Tauri-Response':error?'error':'ok'}});
            if(path.includes('plugin:dialog|open')){t.opens++;return Promise.resolve(response(arguments[2]));}
            if(path.includes('plugin:dialog|message')){t.confirmations++;return Promise.resolve(response('Cancel'));}
            if(path.endsWith('/check_update')){t.checks++;return new Promise(resolve=>{t.finishCheck=()=>resolve(t.failCheck?response('Owned offline check',true):response({version:'99.0.0',notes:'Owned settings check'}));});}
            if(path.endsWith('/download_update')){t.downloads++;return new Promise(resolve=>{t.finishDownload=()=>resolve(response('Owned offline download',true));});}
            return original.call(this,url,options);
          };'''.replace('response(arguments[2])', 'response(' + json.dumps(str(editable)) + ')'))
        click('open-button')
        wait("return document.getElementById('viewer').dataset.phase==='ready' && window.__apertureViewer.world.getItemCount()>0 && !document.querySelector('.point-choice').disabled")
        wait('return window.__apertureViewer.world.getItemAt(0).getTilesToDraw().some(t=>t.tile.loaded)')
        click('points-button')
        point = js('const v=window.__apertureViewer,r=v.canvas.getBoundingClientRect(),b=v.world.getItemAt(0).getBounds(),p=v.viewport.pixelFromPoint(b.getCenter(),true);return {x:Math.round(r.left+p.x),y:Math.round(r.top+p.y)}')
        request('POST', route + '/actions', {'actions': [{'type': 'pointer', 'id': 'mouse',
            'parameters': {'pointerType': 'mouse'}, 'actions': [
                {'type': 'pointerMove', 'origin': 'viewport', **point, 'duration': 0},
                {'type': 'pointerDown', 'button': 0}, {'type': 'pointerUp', 'button': 0}]}]})
        request('DELETE', route + '/actions')
        wait("return document.getElementById('point-count').textContent==='1' && document.getElementById('filename').classList.contains('unsaved')")
        time.sleep(.3)
        before = snapshot()
        open_settings()
        for key in ('e', '\ue012', '\ue014'):
            js("const d=document.getElementById('settings-dialog');d.tabIndex=-1;d.focus()")
            keys(key)
        request('POST', route + '/actions', {'actions': [{'type': 'key', 'id': 'keyboard', 'actions': [
            {'type': 'keyDown', 'value': '\ue009'}, {'type': 'keyDown', 'value': 'o'},
            {'type': 'keyUp', 'value': 'o'}, {'type': 'keyUp', 'value': '\ue009'}]}]})
        request('DELETE', route + '/actions')
        assert snapshot() == before, 'Settings keyboard input changed image, annotations or chooser count'
        js("document.getElementById('settings-general-button').focus()")
        for _ in range(16):
            keys('\ue004')
            assert js("return document.getElementById('settings-dialog').contains(document.activeElement)"), 'Tab escaped modal'
        request('POST', route + '/actions', {'actions': [{'type': 'key', 'id': 'keyboard', 'actions': [
            {'type': 'keyDown', 'value': '\ue008'}, {'type': 'keyDown', 'value': '\ue004'},
            {'type': 'keyUp', 'value': '\ue004'}, {'type': 'keyUp', 'value': '\ue008'}]}]})
        request('DELETE', route + '/actions')
        assert js("return document.getElementById('settings-dialog').contains(document.activeElement)"), 'Shift+Tab escaped modal'
        for preference in ('dark', 'light', 'system'):
            click('settings-appearance-button')
            set_theme(preference)
            assert theme_matches(preference)
            assert snapshot() == before, 'Theme change modified the loaded image or point data'
        escape()
        assert snapshot() == before
        report['checks']['loadedImageAndPointDataPreserved'] = before
        report['checks']['modalKeyboardFocusAndEscape'] = True

        open_settings()
        click('update-check')
        wait('return !!window.__settingsTest.finishCheck')
        assert js("return document.getElementById('settings-close').disabled")
        keys('\ue00c')
        assert js("return document.getElementById('settings-dialog').open"), 'Escape closed busy Settings'
        click('settings-close')
        assert js("return document.getElementById('settings-dialog').open"), 'Close dismissed busy Settings'
        click('settings-appearance-button')
        js('window.__settingsTest.finishCheck()')
        wait("return !document.getElementById('settings-close').disabled")
        assert js("return !document.getElementById('settings-appearance').hidden && document.getElementById('settings-button').classList.contains('update-available')"), 'Update completion changed Appearance or lost the gear dot'
        click('settings-general-button')
        assert js("return document.getElementById('update-status').textContent.includes('99.0.0') && !document.getElementById('update-action').hidden")
        click('update-later')
        wait("return !document.getElementById('settings-dialog').open")
        assert js("return !document.getElementById('settings-button').classList.contains('update-available') && JSON.parse(localStorage.getItem('aperture-updates')).deferredVersion==='99.0.0'")
        open_settings()
        click('update-check')
        wait('return window.__settingsTest.checks===2')
        js('window.__settingsTest.finishCheck()')
        wait("return !document.getElementById('settings-close').disabled")
        assert js("return document.getElementById('settings-button').classList.contains('update-available')"), 'Manual check did not reannounce deferred update'
        click('update-action')
        wait('return !!window.__settingsTest.finishDownload')
        assert js("return !document.getElementById('update-download-progress').hidden && document.getElementById('settings-close').disabled")
        js('window.__settingsTest.finishDownload()')
        wait("return document.getElementById('update-status').textContent.includes('Update failed') && !document.getElementById('settings-close').disabled")
        click('update-action')
        wait('return window.__settingsTest.downloads===2')
        js('window.__settingsTest.finishDownload()')
        wait("return !document.getElementById('settings-close').disabled && document.getElementById('update-download-progress').hidden")
        report['checks']['updateBusyDeferralManualCheckDownloadRetry'] = True
        escape()

        resize(640, 400)
        open_settings()
        for page in ('general', 'appearance'):
            click(f'settings-{page}-button')
            layout = js("""const d=document.getElementById('settings-dialog'),c=document.querySelector('.settings-content'),r=d.getBoundingClientRect();
              return {width:innerWidth,height:innerHeight,dialog:[r.x,r.y,r.width,r.height],
                noOverflow:document.documentElement.scrollWidth<=innerWidth && d.scrollWidth<=d.clientWidth && c.scrollWidth<=c.clientWidth,
                controls:[...d.querySelectorAll('button,select,input')].filter(e=>e.getClientRects().length).every(e=>{const b=e.getBoundingClientRect();return b.left>=0 && b.right<=innerWidth && b.width>0;})};""")
            assert layout['noOverflow'] and layout['controls'], layout
            report['checks'][f'minimumWindow{page.title()}'] = layout
            screenshot(f'settings-{page}-640x400')
        escape()
        open_settings()
        click('settings-window-close-button')
        wait('return window.__settingsTest.confirmations===1')
        assert js("return !document.getElementById('settings-dialog').open && document.getElementById('filename').classList.contains('unsaved') && document.getElementById('point-count').textContent==='1'"), 'Cancelling native Close lost edits or left Settings open'
        open_settings()
        escape()
        report['checks']['nativeTitlebarCloseCancelPreservesEdits'] = True
        assert editable.read_bytes() == original and source.read_bytes() == original, 'Settings changed image bytes'
        report['checks']['fixturesUnchanged'] = True
        report['passed'] = True
    except Exception as error:
        report['error'] = str(error)
        try:
            screenshot('settings-failure')
        except Exception:
            pass
        raise
    finally:
        args.output.write_text(json.dumps(report, indent=2) + '\n')
        try:
            if theme_read:
                js('if(arguments[0]===null)localStorage.removeItem("aperture-theme");else localStorage.setItem("aperture-theme",arguments[0]);', [saved_theme])
            # Only this WebDriver session is closed. The driver and other windows remain owned by their callers.
        finally:
            request('DELETE', route)
    print(json.dumps(report))


if __name__ == '__main__':
    main()
