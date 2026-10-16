#!/usr/bin/env python3
"""Check update failure/unsaved guards in the native Linux app via tauri-driver.

Start tauri-driver with isolated XDG_CACHE_HOME/XDG_CONFIG_HOME and an owned TMPDIR.
Only chooser/confirmation responses and update feed/download are supplied by this
check. Pointer edits, TIFF saving and development-mode install rejection are real.
This does not install an update or validate native dialog rendering.
"""

import argparse
import json
from pathlib import Path
import shutil
import time
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--driver-url', default='http://127.0.0.1:4444')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/update-recovery/result.json')
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    source = ROOT / 'tests/fixtures/gray16-stripped.tif'
    before = source.read_bytes()
    editable = args.output.parent / f'editable-{time.time_ns()}.tif'
    shutil.copyfile(source, editable)

    def request(method, route, body=None):
        payload = None if body is None else json.dumps(body).encode()
        with urlopen(Request(args.driver_url + route, data=payload, method=method,
                             headers={'Content-Type': 'application/json'}), timeout=35) as response:
            value = json.load(response)['value']
        if isinstance(value, dict) and value.get('error'):
            raise RuntimeError(value)
        return value

    session = request('POST', '/session', {'capabilities': {'alwaysMatch': {
        'browserName': 'wry', 'tauri:options': {'application': str(args.binary.resolve())}}}})
    route = '/session/' + session['sessionId']

    def js(code):
        return request('POST', route + '/execute/sync', {'script': code, 'args': []})

    def wait(code):
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            if result := js(code):
                return result
            time.sleep(.1)
        raise TimeoutError(code)

    def click(element):
        js('document.getElementById(' + json.dumps(element) + ').click()')

    def choose(value):
        wait("return document.getElementById('update-unsaved-dialog').open")
        js('document.querySelector(' + json.dumps(
            '#update-unsaved-dialog button[value=' + value + ']') + ').click()')

    def download():
        click('update-action')
        wait("return document.getElementById('update-action').textContent==='Install and restart' && !document.getElementById('update-action').disabled")

    try:
        url = request('GET', route + '/url')
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1'})
        wait('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__')
        js('''const original=window.fetch;window.__updateTest={installs:0,confirmations:0,downloads:0,offline:false};
        window.fetch=function(url,options){
          const path=decodeURIComponent(String(url));let mock=false,value=null;
          if(path.includes('plugin:dialog|open')) {mock=true;value=''' + json.dumps(str(editable)) + ''';}
          if(path.includes('plugin:dialog|message')) {mock=true;value='Cancel';window.__updateTest.confirmations++;}
          if(path.endsWith('/check_update')) {mock=true;value={version:'99.0.0',notes:'Native guard check'};}
          if(path.endsWith('/download_update')) {
            window.__updateTest.downloads++;
            if(window.__updateTest.offline) return Promise.reject(new Error('Update download is offline'));
            mock=true;
          }
          if(path.endsWith('/install_update')) window.__updateTest.installs++;
          if(mock) return Promise.resolve(new Response(JSON.stringify(value),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
          return original.call(this,url,options);
        };''')
        click('open-button')
        wait("return document.getElementById('viewer').dataset.phase==='ready' && window.__apertureViewer.world.getItemCount()>0")
        wait("return !document.querySelector('.point-choice').disabled")
        click('points-button')
        point = js('const v=window.__apertureViewer,r=v.canvas.getBoundingClientRect(),b=v.world.getItemAt(0).getBounds(),p=v.viewport.pixelFromPoint(b.getCenter(),true);return {x:Math.round(r.left+p.x),y:Math.round(r.top+p.y)}')
        request('POST', route + '/actions', {'actions': [{'type': 'pointer', 'id': 'mouse',
            'parameters': {'pointerType': 'mouse'}, 'actions': [
                {'type': 'pointerMove', 'origin': 'viewport', **point, 'duration': 0},
                {'type': 'pointerDown', 'button': 0}, {'type': 'pointerUp', 'button': 0}]}]})
        request('DELETE', route + '/actions')
        wait("return document.getElementById('filename').classList.contains('unsaved')")
        click('settings-button')
        click('update-check')
        wait("return !document.getElementById('update-action').hidden && !document.getElementById('update-action').disabled")
        download()
        js('window.__updateTest.offline=true')
        click('update-action')
        choose('cancel')
        wait("return document.getElementById('settings-dialog').open && !document.getElementById('update-action').disabled")
        assert js("return document.getElementById('filename').classList.contains('unsaved') && window.__updateTest.installs===0")
        click('update-action')
        choose('discard')
        wait("return document.getElementById('update-status').textContent.includes('Update failed') && !document.getElementById('update-action').disabled")
        assert js("return document.getElementById('filename').classList.contains('unsaved') && window.__updateTest.installs===1")
        click('settings-close')
        assert js("return !document.querySelector('.point-choice').disabled")
        click('open-button')
        wait('return window.__updateTest.confirmations===1')
        assert js("return document.getElementById('filename').classList.contains('unsaved') && document.getElementById('point-count').textContent==='1'")
        click('settings-button')
        click('update-action')
        retry = wait("const t=window.__updateTest,prompt=document.getElementById('update-unsaved-dialog').open;return prompt || t.downloads>1 ? {prompt,downloads:t.downloads,installs:t.installs,action:document.getElementById('update-action').textContent} : null")
        assert retry['prompt'] and retry['downloads'] == 1 and retry['installs'] == 1, (
            'Install retry must use the downloaded update while offline: ' + json.dumps(retry))
        choose('save')
        wait("return document.getElementById('save-dialog').open")
        click('save-overwrite-button')
        wait("return document.getElementById('update-status').textContent.includes('Update failed') && window.__updateTest.installs===2")
        assert js("return !document.getElementById('filename').classList.contains('unsaved')")
        assert js('return window.__updateTest.downloads===1 && window.__updateTest.offline')
        assert editable.read_bytes() != before, 'Native annotation save did not write the owned copy'
        assert source.read_bytes() == before, 'Source fixture changed'
        result = {'cancelPreservesEdits': True, 'failedDiscardInstallPreservesDirty': True,
                  'subsequentOpenWarns': True, 'editingRestored': True, 'savePersistsAnnotations': True,
                  'nativeDevelopmentInstallRejected': True,
                  'offlineInstallRetryUsesDownloadedUpdate': True, 'downloadAttempts': 1,
                  'mocked': ['file chooser', 'confirmation response', 'update metadata', 'download']}
        args.output.write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        request('DELETE', route)


if __name__ == '__main__':
    main()
