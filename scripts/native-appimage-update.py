#!/usr/bin/env python3
"""Exercise real AppImage check/download/install IPC against an owned local feed.

The prior package must be a RELEASE build with version 0.0.9, the production
public key, requireSignedVersion=true, and a test-only endpoint matching
http://127.0.0.1:8763/latest.json. Only this prior package permits local HTTP.
Serve the exact production candidate and its signature; neither input is edited.
Start tauri-driver with isolated XDG_CACHE_HOME/XDG_CONFIG_HOME, an owned TMPDIR,
and APPIMAGE_EXTRACT_AND_RUN=1. Run this check as a regular desktop user.

Only chooser/confirmation responses are supplied. Update IPC, signature checks,
annotation editing/saving, replacement, failure recovery and restart are real.
"""

import argparse
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import stat
import threading
import time
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    hashed = hashlib.sha256()
    with path.open('rb') as source:
        while block := source.read(1024 * 1024):
            hashed.update(block)
    return hashed.hexdigest()


def owned_processes(appimage):
    result = set()
    marker = b'APPIMAGE=' + os.fsencode(appimage)
    for process in Path('/proc').iterdir():
        if not process.name.isdecimal():
            continue
        try:
            if process.stat().st_uid == os.getuid() and marker in (process / 'environ').read_bytes().split(b'\0'):
                result.add(int(process.name))
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            pass
    return result


def stop_owned(appimage):
    for pid in owned_processes(appimage):
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 5
    while owned_processes(appimage) and time.monotonic() < deadline:
        time.sleep(.1)
    if owned_processes(appimage):
        raise RuntimeError('Owned update-test processes did not stop after SIGTERM')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prior', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--signature', type=Path, required=True)
    parser.add_argument('--version', required=True, help='Exact signed candidate version')
    parser.add_argument('--prior-version', default='0.0.9')
    parser.add_argument('--feed-port', type=int, default=8763)
    parser.add_argument('--driver-url', default='http://127.0.0.1:4444')
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/appimage-update/result.json')
    args = parser.parse_args()
    if os.geteuid() == 0:
        raise RuntimeError('Run as a regular user so read-only directory permissions are enforced')
    args.candidate = args.candidate.resolve()
    args.prior = args.prior.resolve()
    candidate_hash, prior_hash = digest(args.candidate), digest(args.prior)
    signature = args.signature.read_text(encoding='utf8').strip()
    if not signature:
        raise RuntimeError('A production candidate signature is required')
    args.output = args.output.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    run = args.output.parent / f'owned-{time.time_ns()}'
    run.mkdir()
    appimage = run / 'Aperture-update-test.AppImage'
    shutil.copyfile(args.prior, appimage)
    appimage.chmod(0o755)
    editable = args.output.parent / f'editable-{time.time_ns()}.tif'
    source = ROOT / 'tests/fixtures/gray16-stripped.tif'
    source_hash = digest(source)
    shutil.copyfile(source, editable)
    fixture = {'mode': 'offline', 'version': args.version, 'checks': 0, 'downloads': 0}

    class Feed(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            if self.path == '/latest.json':
                fixture['checks'] += 1
            if fixture['mode'] == 'offline':
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                return
            if self.path == '/latest.json':
                body = json.dumps({'version': fixture['version'], 'notes': 'Owned signed AppImage acceptance test',
                    'platforms': {'linux-x86_64': {
                        'url': f'http://127.0.0.1:{args.feed_port}/candidate.AppImage', 'signature': signature}}}).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif self.path == '/candidate.AppImage':
                fixture['downloads'] += 1
                self.send_response(200)
                self.send_header('Content-Type', 'application/octet-stream')
                self.send_header('Content-Length', str(args.candidate.stat().st_size))
                self.end_headers()
                with args.candidate.open('rb') as package:
                    first = True
                    while block := package.read(256 * 1024):
                        if first and fixture['mode'] == 'tamper':
                            block = block[:128] + bytes([block[128] ^ 1]) + block[129:]
                        first = False
                        self.wfile.write(block)
            else:
                self.send_error(404)

    feed = ThreadingHTTPServer(('127.0.0.1', args.feed_port), Feed)
    thread = threading.Thread(target=feed.serve_forever, daemon=True)
    thread.start()
    route = None

    def request(method, suffix, body=None):
        payload = None if body is None else json.dumps(body).encode()
        with urlopen(Request(args.driver_url + suffix, data=payload, method=method,
                             headers={'Content-Type': 'application/json'}), timeout=40) as response:
            value = json.load(response)['value']
        if isinstance(value, dict) and value.get('error'):
            raise RuntimeError(value)
        return value

    def session():
        session = request('POST', '/session', {'capabilities': {'alwaysMatch': {
            'browserName': 'wry', 'tauri:options': {'application': str(appimage)}}}})
        return '/session/' + session['sessionId']

    def js(code):
        return request('POST', route + '/execute/sync', {'script': code, 'args': []})

    def wait(code, timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if result := js(code):
                return result
            time.sleep(.1)
        raise TimeoutError(code)

    def click(element):
        # Prior releases still have the standalone update dialog.
        js('const id=' + json.dumps(element) + ";const old={'settings-button':'update-button','settings-close':'update-close'};(document.getElementById(id)||document.getElementById(old[id])).click()")

    def check():
        click('update-check')
        wait("return !document.getElementById('update-action').hidden && !document.getElementById('update-action').disabled")

    def downloaded():
        click('update-action')
        wait("return document.getElementById('update-action').textContent==='Install and restart' && !document.getElementById('update-action').disabled", 90)

    def failed():
        wait("return document.getElementById('update-status').textContent.includes('Update failed') && !document.getElementById('update-action').disabled", 90)

    def choose(value):
        wait("return document.getElementById('update-unsaved-dialog').open")
        js('document.querySelector(' + json.dumps('#update-unsaved-dialog button[value=' + value + ']') + ').click()')

    try:
        route = session()
        wait("return document.getElementById('update-edition').textContent.includes('Version ')")
        assert js('return document.getElementById("update-edition").textContent') == f'Version {args.prior_version} · Linux AppImage'
        url = request('GET', route + '/url')
        js('localStorage.removeItem("aperture-updates")')
        request('POST', route + '/url', {'url': url + '?colorBenchmark=1'})
        wait('return !!window.__apertureViewer && !!window.__TAURI_INTERNALS__')
        js('''const original=window.fetch;window.__signedUpdateTest={confirmations:0};
        window.fetch=function(url,options){
          const path=decodeURIComponent(String(url));let mock=false,value=null;
          if(path.includes('plugin:dialog|open')) {mock=true;value=''' + json.dumps(str(editable)) + ''';}
          if(path.includes('plugin:dialog|message')) {mock=true;value='Cancel';window.__signedUpdateTest.confirmations++;}
          if(mock) return Promise.resolve(new Response(JSON.stringify(value),{headers:{'Content-Type':'application/json','Tauri-Response':'ok'}}));
          return original.call(this,url,options);
        };''')
        click('open-button')
        wait("return document.getElementById('viewer').dataset.phase==='ready' && !document.querySelector('.point-choice').disabled")
        click('points-button')
        point = js('const v=window.__apertureViewer,r=v.canvas.getBoundingClientRect(),b=v.world.getItemAt(0).getBounds(),p=v.viewport.pixelFromPoint(b.getCenter(),true);return {x:Math.round(r.left+p.x),y:Math.round(r.top+p.y)}')
        request('POST', route + '/actions', {'actions': [{'type': 'pointer', 'id': 'mouse',
            'parameters': {'pointerType': 'mouse'}, 'actions': [
                {'type': 'pointerMove', 'origin': 'viewport', **point, 'duration': 0},
                {'type': 'pointerDown', 'button': 0}, {'type': 'pointerUp', 'button': 0}]}]})
        request('DELETE', route + '/actions')
        wait("return document.getElementById('filename').classList.contains('unsaved')")
        deadline = time.monotonic() + 20
        while fixture['checks'] == 0 and time.monotonic() < deadline:
            time.sleep(.1)
        assert fixture['checks'] > 0, 'Automatic startup check did not reach the offline fixture'
        wait("return !document.getElementById('update-check').disabled")
        assert js("return !(document.getElementById('settings-dialog') || document.getElementById('update-dialog')).open && document.getElementById('viewer').dataset.phase==='ready'")
        click('settings-button')
        click('update-check')
        wait("return document.getElementById('update-status').textContent.includes('Could not check') && !document.getElementById('update-check').disabled")

        fixture['mode'] = 'tamper'
        check()
        click('update-action')
        failed()
        assert digest(appimage) == prior_hash, 'Tampered download changed the installed AppImage'
        assert js("return document.getElementById('update-action').textContent==='Download update'")
        fixture['mode'] = 'normal'
        fixture['version'] = '99.0.0'
        check()
        click('update-action')
        failed()
        assert digest(appimage) == prior_hash, 'Version-mismatched signature changed the installed AppImage'
        fixture['version'] = args.version
        check()
        downloaded()
        click('update-action')
        choose('cancel')
        wait("return (document.getElementById('settings-dialog') || document.getElementById('update-dialog')).open && !document.getElementById('update-action').disabled")
        assert digest(appimage) == prior_hash
        assert js("return document.getElementById('filename').classList.contains('unsaved')")

        run.chmod(0o555)
        click('update-action')
        choose('discard')
        failed()
        assert digest(appimage) == prior_hash, 'Failed replacement changed the installed AppImage'
        assert js("return document.getElementById('filename').classList.contains('unsaved')")
        click('settings-close')
        assert js("return !document.querySelector('.point-choice').disabled")
        click('open-button')
        wait('return window.__signedUpdateTest.confirmations===1')
        assert js("return document.getElementById('filename').classList.contains('unsaved')")
        run.chmod(0o755)
        click('settings-button')
        assert js("return document.getElementById('update-action').textContent==='Install and restart'")
        checks, downloads = fixture['checks'], fixture['downloads']
        fixture['mode'] = 'offline'
        original_processes = owned_processes(appimage)
        assert original_processes, 'Could not identify the owned AppImage process'
        click('update-action')
        choose('save')
        wait("return document.getElementById('save-dialog').open")
        click('save-overwrite-button')
        deadline = time.monotonic() + 45
        while digest(appimage) != candidate_hash and time.monotonic() < deadline:
            time.sleep(.1)
        assert digest(appimage) == candidate_hash, 'Installed bytes do not match the production candidate'
        assert (fixture['checks'], fixture['downloads']) == (checks, downloads), 'Retry required network instead of retained verified bytes'
        assert stat.S_IMODE(appimage.stat().st_mode) == 0o755
        assert digest(editable) != source_hash, 'Native annotation save did not write the owned TIFF'
        deadline = time.monotonic() + 20
        restarted = set()
        while time.monotonic() < deadline:
            restarted = owned_processes(appimage) - original_processes
            if restarted:
                break
            time.sleep(.1)
        assert restarted, 'No automatic restart process appeared for the updated AppImage'
        try:
            request('DELETE', route)
        except Exception:
            pass
        route = None
        stop_owned(appimage)
        route = session()
        wait("return document.getElementById('update-edition').textContent.includes('Version ')")
        assert js('return document.getElementById("update-edition").textContent') == f'Version {args.version} · Linux AppImage'
        screenshot = request('GET', route + '/screenshot')
        (args.output.parent / 'upgraded-native.png').write_bytes(base64.b64decode(screenshot))
        assert digest(args.candidate) == candidate_hash and digest(args.prior) == prior_hash
        assert digest(source) == source_hash, 'Source fixture changed'
        result = {'priorVersion': args.prior_version, 'installedVersion': args.version,
            'candidateSha256': candidate_hash, 'offlineAutomaticCheckQuiet': True,
            'offlineManualCheckRecoverable': True, 'tamperedArtifactRejected': True,
            'signedVersionMismatchRejected': True, 'cancelPreservesEdits': True,
            'readOnlyInstallFailurePreservesBytesAndDirty': True, 'subsequentOpenWarns': True,
            'offlineRetryWithRetainedVerifiedBytes': True, 'saveBeforeUpgradeWritesAnnotations': True,
            'installedExactCandidateBytes': True, 'automaticRestartObserved': True,
            'updatedNativeVersionVerified': True, 'checkDownloadInstallIpcUnmocked': True,
            'mocked': ['file chooser', 'confirmation response'],
            'fixtureRequests': {'checks': fixture['checks'], 'downloads': fixture['downloads']}}
        args.output.write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        run.chmod(0o755)
        if route:
            try:
                request('DELETE', route)
            except Exception:
                pass
        stop_owned(appimage)
        feed.shutdown()
        feed.server_close()


if __name__ == '__main__':
    main()
