#!/usr/bin/env python3
"""Fixed screenshot and harmless Shift wake for the exact handed-off Fedora QEMU."""
import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import socket
import time

def process(pid, directory):
    proc = Path('/proc', str(pid))
    if proc.stat().st_uid != os.getuid() or (proc / 'exe').resolve().name != 'qemu-system-x86_64':
        raise ValueError('Owned QEMU identity changed')
    fields = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    if fields[0] == 'Z' or not (proc / 'root/work').samefile(directory):
        raise ValueError('Owned QEMU work directory changed')
    return fields[19]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pid', type=int, required=True)
    parser.add_argument('--port', type=int, default=18983)
    parser.add_argument('--work-directory', type=Path, required=True, help='Exact QEMU /work bind directory')
    parser.add_argument('--qmp', type=Path, required=True, help='Existing owned QMP socket')
    args = parser.parse_args()
    directory = args.work_directory.resolve()
    qmp = args.qmp.resolve()
    if qmp.parent != directory:
        raise ValueError('QMP socket must belong to the exact owned work directory')
    ticks = process(args.pid, directory)
    os.chdir(directory)

    class Capture(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path not in ('/frame.ppm', '/wake-frame.ppm'):
                self.send_error(404)
                return
            try:
                if process(args.pid, directory) != ticks:
                    raise ValueError('Owned QEMU PID reused')
                with socket.socket(socket.AF_UNIX) as sock:
                    sock.settimeout(8)
                    sock.connect(qmp.name)
                    stream = sock.makefile('rwb', buffering=0)
                    json.loads(stream.readline())
                    def command(name, arguments=None):
                        stream.write((json.dumps({'execute': name, 'arguments': arguments or {}}) + '\n').encode())
                        while True:
                            result = json.loads(stream.readline())
                            if 'error' in result:
                                raise RuntimeError(result['error'])
                            if 'return' in result:
                                return
                    command('qmp_capabilities')
                    if self.path == '/wake-frame.ppm':
                        command('send-key', {'keys': [{'type': 'qcode', 'data': 'shift'}], 'hold-time': 50})
                        time.sleep(.7)
                    command('screendump', {'filename': '/work/native-capture.ppm'})
                data = (directory / 'native-capture.ppm').read_bytes()
                if not data.startswith(b'P6\n'):
                    raise ValueError('QMP capture is not P6')
                self.send_response(200)
                self.send_header('Content-Type', 'image/x-portable-pixmap')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception as error:
                self.send_error(503, str(error))

    HTTPServer(('127.0.0.1', args.port), Capture).serve_forever()


if __name__ == '__main__':
    main()
