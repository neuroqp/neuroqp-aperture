"""Compositor access for native GUI checks. No Python packages required.

Native scripts run inside scripts/headless-session (nested Sway, the default) or,
only when explicitly requested, scripts/visible-session (the real Hyprland
desktop). Visible mode never injects input, focuses windows, reads the
clipboard or captures the screen unless APERTURE_VISIBLE_INPUT=1 is set.
"""

import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time

# GTK derives the Wayland app_id from argv[0]; see application().
APP_ID = 'aperture-test'

# Linux input event codes and XKB modifier masks for keys() combos.
KEYCODES = {'Escape': 1, 'Tab': 15, 'Return': 28, 'ctrl': 29, 'a': 30, 'l': 38,
            'shift': 42, 'v': 47, 'alt': 56}
MODIFIERS = {'shift': 1, 'ctrl': 4, 'alt': 8}


class SessionError(RuntimeError):
    pass


def session():
    """Return the compositor backend for the current test session."""
    mode = os.environ.get('APERTURE_SESSION')
    if mode == 'headless' and os.environ.get('SWAYSOCK'):
        return Sway()
    if mode == 'visible' and os.environ.get('HYPRLAND_INSTANCE_SIGNATURE'):
        return Hyprland()
    raise SessionError(
        'Run native GUI checks inside scripts/headless-session, or opt into the '
        'real desktop with scripts/visible-session.')


def application(binary):
    """Return a launcher for binary whose windows use the aperture-test app_id."""
    session()
    link = Path(os.environ['APERTURE_TEST_ROOT']) / 'bin' / APP_ID
    link.parent.mkdir(parents=True, exist_ok=True)
    link.unlink(missing_ok=True)
    link.symlink_to(Path(binary).resolve())
    return link


def run(*command, **options):
    return subprocess.run(command, check=True, stdout=subprocess.DEVNULL, **options)


def combo_codes(combo):
    """Split 'ctrl+l' into its key codes and modifier mask."""
    *modifiers, key = combo.split('+')
    return [KEYCODES[m] for m in modifiers], KEYCODES[key], sum(MODIFIERS[m] for m in modifiers)


class Sway:
    """Nested headless Sway: windows, input and clipboard are private."""

    def windows(self):
        found = []

        def walk(node):
            for child in node.get('nodes', []) + node.get('floating_nodes', []):
                walk(child)
            if node.get('pid'):
                rect, content = node['rect'], node['window_rect']
                found.append({
                    'id': node['id'], 'app_id': node.get('app_id') or '',
                    'title': node.get('name') or '', 'pid': node['pid'],
                    'x': rect['x'] + content['x'], 'y': rect['y'] + content['y'],
                    'width': content['width'], 'height': content['height'],
                    'focused': node['focused'], 'floating': node['type'] == 'floating_con'})

        walk(json.loads(subprocess.check_output(['swaymsg', '-t', 'get_tree', '-r'])))
        return found

    def _command(self, window, command):
        run('swaymsg', f'[con_id={window["id"]}] {command}')

    def focus(self, window):
        self._command(window, 'focus')

    def float(self, window):
        self._command(window, 'floating enable')

    def resize(self, window, width, height):
        """Float (tiling ignores size requests) and resize the window content."""
        self._command(window, f'floating enable, resize set {width} px {height} px')

    def keys(self, *combos):
        """Press combos such as 'ctrl+l' or 'Return'; see KEYCODES."""
        with Wayland() as wayland:
            wayland.keys(combos)

    def type_text(self, text):
        # wtype's generated keymap suits text, not GTK shortcuts; see Wayland.keys.
        run('wtype', '-s', '300', '--', text)

    def click(self, x, y):
        """Left-click at layout coordinates.

        Headless Sway has no pointer device, so `swaymsg seat - cursor` events
        never reach clients. A temporary wlr virtual pointer adds one.
        """
        outputs = json.loads(subprocess.check_output(['swaymsg', '-t', 'get_outputs', '-r']))
        width = max(o['rect']['x'] + o['rect']['width'] for o in outputs if o['active'])
        height = max(o['rect']['y'] + o['rect']['height'] for o in outputs if o['active'])
        with Wayland() as wayland:
            wayland.click(x, y, width, height)

    def screenshot(self, path, region=None):
        """Capture the nested output; region is (x, y, width, height)."""
        geometry = [] if region is None else ['-g', '{},{} {}x{}'.format(*map(round, region))]
        run('grim', *geometry, str(path))

    def clipboard(self):
        """Return clipboard bytes, or None when it is empty or unavailable."""
        result = subprocess.run(['wl-paste', '--no-newline'], stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL)
        return result.stdout if result.returncode == 0 else None

    def set_clipboard(self, data):
        run('wl-copy', input=data)


class Hyprland(Sway):
    """The user's real Hyprland desktop. Only test-owned windows are changed."""

    def __init__(self):
        self.input = os.environ.get('APERTURE_VISIBLE_INPUT') == '1'

    def _desktop(self, action):
        if not self.input:
            raise SessionError(
                f'{action} would act on the real desktop. Use scripts/headless-session, '
                'or set APERTURE_VISIBLE_INPUT=1 to allow it in visible mode.')

    def windows(self):
        return [{
            'id': w['address'], 'app_id': w['class'], 'title': w['title'], 'pid': w['pid'],
            'x': w['at'][0], 'y': w['at'][1], 'width': w['size'][0], 'height': w['size'][1],
            'focused': w['focusHistoryID'] == 0, 'floating': w['floating'],
        } for w in json.loads(subprocess.check_output(['hyprctl', '-j', 'clients']))
            if w.get('mapped')]

    def _dispatch(self, window, *dispatchers):
        if window['app_id'] != APP_ID:
            raise SessionError(f'Refusing to change a window not owned by the test: {window}')
        selector = json.dumps('address:' + window['id'])
        run('hyprctl', 'eval', ';'.join(
            f'hl.dispatch(hl.dsp.{d.replace("WINDOW", selector)})' for d in dispatchers))

    def focus(self, window):
        self._desktop('Focusing a window')
        self._dispatch(window, 'focus({window=WINDOW})')

    def float(self, window):
        self._dispatch(window, 'window.float({action="set",window=WINDOW})')

    def resize(self, window, width, height):
        self._dispatch(window, 'window.float({action="set",window=WINDOW})',
                       f'window.resize({{x={width},y={height},relative=false,window=WINDOW}})')

    def _ydotool(self, *args):
        socket_path = Path(os.environ['XDG_RUNTIME_DIR']) / '.ydotool_socket'
        run('ydotool', *args, env={'YDOTOOL_SOCKET': str(socket_path), **os.environ})

    def keys(self, *combos):
        self._desktop('Keyboard input')
        for combo in combos:
            modifiers, key, _ = combo_codes(combo)
            codes = [*modifiers, key]
            self._ydotool('key', *[f'{c}:1' for c in codes], *[f'{c}:0' for c in reversed(codes)])

    def type_text(self, text):
        self._desktop('Keyboard input')
        self._ydotool('type', '--', text)

    def click(self, x, y):
        self._desktop('Pointer input')
        run('hyprctl', 'eval', f'hl.dispatch(hl.dsp.cursor.move({{x = {x}, y = {y}}}))')
        self._ydotool('click', '0xC0')

    def screenshot(self, path, region=None):
        self._desktop('A desktop screenshot')
        super().screenshot(path, region)

    def clipboard(self):
        self._desktop('Reading the clipboard')
        return super().clipboard()

    def set_clipboard(self, data):
        self._desktop('Changing the clipboard')
        super().set_clipboard(data)


class Wayland:
    """Minimal client adding a temporary virtual keyboard or pointer to the
    current WAYLAND_DISPLAY. Headless Sway has no input devices of its own."""

    # A standard US layout, so GTK's shortcut tables (built from the default
    # evdev keymap) match. wtype's per-run keymaps break Return, Escape and Ctrl
    # shortcuts in GTK dialogs, while text still works.
    KEYMAP = b'''xkb_keymap {
        xkb_keycodes { include "evdev+aliases(qwerty)" };
        xkb_types { include "complete" };
        xkb_compatibility { include "complete" };
        xkb_symbols { include "pc+us+inet(evdev)" };
    };\0'''

    def __enter__(self):
        path = Path(os.environ['XDG_RUNTIME_DIR']) / os.environ['WAYLAND_DISPLAY']
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.connect(str(path))
        self.buffer, self.last_id = b'', 1
        self.registry = self.new_id()
        self.send(1, 1, self.registry)  # wl_display.get_registry
        self.globals = {}
        for body in self.roundtrip(self.registry):  # wl_registry.global events
            name, length = struct.unpack_from('<II', body)
            self.globals[body[8:8 + length - 1].decode()] = name
        return self

    def __exit__(self, *_):
        self.socket.close()

    def new_id(self):
        self.last_id += 1
        return self.last_id

    def send(self, target, opcode, *args, fds=()):
        body = b''.join(a if isinstance(a, bytes) else struct.pack('<I', a) for a in args)
        message = struct.pack('<IHH', target, opcode, 8 + len(body)) + body
        socket.send_fds(self.socket, [message], fds) if fds else self.socket.sendall(message)

    def bind(self, interface, version=1):
        if interface not in self.globals:
            raise SessionError(f'The compositor does not offer {interface}')
        name = interface.encode() + b'\0'
        name += b'\0' * (-len(name) % 4)
        bound = self.new_id()
        self.send(self.registry, 0, self.globals[interface], len(interface) + 1, name,
                  version, bound)
        return bound

    def roundtrip(self, source=None):
        """Wait until the compositor has handled all requests; return the
        bodies of event 0 sent to source meanwhile."""
        callback = self.new_id()
        self.send(1, 0, callback)  # wl_display.sync
        events = []
        while True:
            while len(self.buffer) < 8 or len(self.buffer) < struct.unpack_from('<IHH', self.buffer)[2]:
                data = self.socket.recv(65536)
                if not data:
                    raise SessionError('Wayland connection closed')
                self.buffer += data
            target, opcode, size = struct.unpack_from('<IHH', self.buffer)
            body, self.buffer = self.buffer[8:size], self.buffer[size:]
            if target == 1 and opcode == 0:
                raise SessionError(f'Wayland protocol error: {body!r}')
            if target == callback:
                return events
            if target == source and opcode == 0:
                events.append(body)

    @staticmethod
    def time():
        return int(time.monotonic() * 1000) & 0xFFFFFFFF

    def keys(self, combos):
        # New object ids must be sent in increasing order.
        seat = self.bind('wl_seat')
        manager = self.bind('zwp_virtual_keyboard_manager_v1')
        keyboard = self.new_id()
        self.send(manager, 0, seat, keyboard)
        with tempfile.TemporaryFile() as keymap:
            keymap.write(self.KEYMAP)
            keymap.flush()
            self.send(keyboard, 0, 1, len(self.KEYMAP), fds=[keymap.fileno()])  # xkb_v1
            self.roundtrip()
        time.sleep(0.3)  # Let clients bind the new keyboard capability.

        def key(code, pressed):
            self.send(keyboard, 1, self.time(), code, int(pressed))
            time.sleep(0.02)

        for combo in combos:
            modifiers, code, mask = combo_codes(combo)
            for modifier in modifiers:
                key(modifier, True)
            self.send(keyboard, 2, mask, 0, 0, 0)  # modifiers
            key(code, True)
            key(code, False)
            for modifier in reversed(modifiers):
                key(modifier, False)
            self.send(keyboard, 2, 0, 0, 0, 0)
        self.roundtrip()
        time.sleep(0.2)  # Let clients handle the keys before the device goes.
        self.send(keyboard, 3)  # destroy
        self.roundtrip()

    def click(self, x, y, width, height):
        manager = self.bind('zwlr_virtual_pointer_manager_v1')
        pointer = self.new_id()
        self.send(manager, 0, 0, pointer)  # seat=null
        self.roundtrip()
        time.sleep(0.3)  # Let clients bind the new pointer capability.
        self.send(pointer, 1, self.time(), round(x), round(y), width, height)  # motion_absolute
        self.send(pointer, 4)  # frame
        time.sleep(0.1)
        for pressed in (1, 0):
            self.send(pointer, 2, self.time(), 0x110, pressed)  # BTN_LEFT
            self.send(pointer, 4)
            time.sleep(0.05)
        self.roundtrip()
        time.sleep(0.2)
        self.send(pointer, 8)  # destroy
        self.roundtrip()
