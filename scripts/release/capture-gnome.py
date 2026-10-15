#!/usr/bin/env python3
"""Crop QMP framebuffer to the live-confirmed maximized GNOME owned frame."""
import argparse
import io
import json
import os
from pathlib import Path
import time
from urllib.request import urlopen


def crop(data, width, height):
    stream = io.BytesIO(data)
    def token():
        while True:
            line = stream.readline()
            if not line.startswith(b'#'):
                return line.strip()
    if token() != b'P6':
        raise ValueError('Expected P6 screenshot')
    full_width, full_height = map(int, token().split())
    if token() != b'255':
        raise ValueError('Expected 8-bit screenshot')
    pixels = stream.read()
    top = full_height - height
    if len(pixels) != full_width * full_height * 3 or width != full_width or not (0 <= top < full_height) or height <= 0:
        raise ValueError('Live-confirmed maximized frame no longer matches framebuffer')
    return f'P6\n{width} {height}\n255\n'.encode() + pixels[top * full_width * 3:]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pid', type=int, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--bridge-url', required=True, help='Prepared native QMP capture bridge, e.g. http://10.0.2.2:18983')
    args = parser.parse_args()
    proc = Path('/proc', str(args.pid))
    if proc.stat().st_uid != os.getuid() or (proc / 'exe').resolve().name != 'aperture':
        raise ValueError('Expected owned native Aperture process')
    # Wake the exact pinned guest without changing its idle settings or focus.
    with urlopen(args.bridge_url.rstrip('/') + '/wake-frame.ppm', timeout=10) as response:
        wake_frame = response.read(32 * 1024 * 1024)
    if not wake_frame.startswith(b'P6\n'):
        raise ValueError('Fresh native wake framebuffer missing')
    import gi
    gi.require_version('Atspi', '2.0')
    from gi.repository import Atspi
    Atspi.set_timeout(1000, 1000)
    desktop = Atspi.get_desktop(0)
    apps = []
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app and app.get_process_id() == args.pid:
            apps.append(app)
    if len(apps) != 1:
        raise ValueError('Expected one owned native accessibility application')
    def nodes(node, depth=0):
        if depth > 16:
            return
        yield node
        for i in range(node.get_child_count()):
            child = node.get_child_at_index(i)
            if child:
                yield from nodes(child, depth + 1)
    maximize = [node for node in nodes(apps[0]) if node.get_name() in ('Maximize', 'Restore') and node.get_role_name() in ('button', 'push button')]
    if len(maximize) != 1:
        raise ValueError('Expected one owned native Maximize button')
    # A maximized window exposes Restore instead, so never toggle it back.
    if maximize[0].get_name() == 'Maximize':
        state = maximize[0].get_state_set()
        if not state.contains(Atspi.StateType.SENSITIVE):
            raise ValueError('Native Maximize is disabled')
        action = maximize[0].get_action_iface()
        if not action or not action.do_action(0):
            raise ValueError('Native Maximize action rejected')
        time.sleep(.3)
    frames = []
    for attempt in range(15):
        frames = []
        for j in range(apps[0].get_child_count()):
            frame = apps[0].get_child_at_index(j)
            state = frame.get_state_set()
            if frame.get_role_name() in ('frame', 'window') and state.contains(Atspi.StateType.SHOWING) and state.contains(Atspi.StateType.ACTIVE):
                frames.append(frame.get_component_iface().get_extents(Atspi.CoordType.SCREEN))
        if len(frames) == 1 and (frames[0].x, frames[0].y) == (0, 0):
            break
        time.sleep(.1)
    observed = []
    for j in range(apps[0].get_child_count()):
        frame = apps[0].get_child_at_index(j)
        state = frame.get_state_set()
        component = frame.get_component_iface()
        extent = component.get_extents(Atspi.CoordType.SCREEN) if component else None
        observed.append(dict(name=frame.get_name(), role=frame.get_role_name(), showing=state.contains(Atspi.StateType.SHOWING), active=state.contains(Atspi.StateType.ACTIVE), rect=[extent.x, extent.y, extent.width, extent.height] if extent else None))
    Path(str(args.output) + '.geometry.json').write_text(json.dumps(dict(frames=observed, maximize_label=maximize[0].get_name(), maximize_sensitive=maximize[0].get_state_set().contains(Atspi.StateType.SENSITIVE)), indent=2) + '\n')
    if len(frames) != 1 or (frames[0].x, frames[0].y) != (0, 0):
        raise ValueError('Expected one active owned maximized window-local frame')
    if maximize[0].get_name() != 'Restore':
        raise ValueError('Owned window did not confirm maximized Restore state')
    # GNOME reports window-local extents. The active maximized full-width frame
    # fills the framebuffer below its top panel; derive that panel height.
    with urlopen(args.bridge_url.rstrip('/') + '/frame.ppm', timeout=10) as response:
        data = response.read(32 * 1024 * 1024)
    args.output.write_bytes(crop(data, frames[0].width, frames[0].height))


if __name__ == '__main__':
    main()
