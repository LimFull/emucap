#!/usr/bin/env python3
"""Exercise actual patched debugger transactions with controlled native dependencies.

--sources contains ppsspp/, pcsx2/ and mame-pc98/ trees after applying their complete
maintained patch stacks. The fixture runs no emulator or guest.
"""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sources', type=Path, required=True)
parser.add_argument('--windows-objects', type=Path)
parser.add_argument('--adapter', action='append', choices=('ppsspp', 'pcsx2', 'mame'))
args = parser.parse_args()


def function(source, start):
    begin = source.index(start)
    brace = source.index('{', begin)
    # Ignore braces in comments and literals, while preserving source offsets.
    masked = re.sub(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'',
                    lambda m: ' ' * len(m.group()), source, flags=re.S)
    depth = 0
    for end in range(brace, len(source)):
        depth += (masked[end] == '{') - (masked[end] == '}')
        if depth == 0:
            return source[begin:end + 1]
    raise ValueError('Unclosed function: ' + start)


psp = (args.sources / 'ppsspp/Core/Debugger/WebSocket/BreakpointSubscriber.cpp').read_text()
ps2 = (args.sources / 'pcsx2/pcsx2/PINE.cpp').read_text()
mame = args.sources / 'mame-pc98'
header = (mame / 'src/emu/debug/debugcpu.h').read_text()
body = (mame / 'src/emu/debug/debugcpu.cpp').read_text()
gdb = (mame / 'src/osd/modules/debugger/debuggdbstub.cpp').read_text()
fragments = {
    'ppsspp-functions.inc': (function(psp, 'static bool AdmitRemoteBreakpoint(')
                            if 'static bool AdmitRemoteBreakpoint(' in psp else '')
        + '\n' + '\n'.join(function(psp, start) for start in ('void WebSocketCPUBreakpointAdd(',
        'void WebSocketMemoryBreakpointAdd(')),
    'pcsx2-functions.inc': '\n'.join(
        'bool ' + method + '(u32 kind, u32 start, u32 end) { bool ' + result + ' = false;\n'
        + function(ps2, 'Host::RunOnCPUThread([kind, start, end, &' + result + ']')
        + ', true); return ' + result + '; }'
        for method, result in [('add', 'armed'), ('clear', 'cleared')]),
    'mame-allocator.inc': '\n'.join(function(header, 'u32 get_' + kind + '_index()')
                                 for kind in ('breakpoint', 'watchpoint', 'registerpoint', 'exceptionpoint'))
        + '\n' + (function(header, 'static u32 allocate_point_index(')
                   if 'static u32 allocate_point_index(' in header else ''),
    'mame-functions.inc': '\n'.join(function(body, 'int device_debug::' + kind + '_set(')
                                 for kind in ('breakpoint', 'watchpoint', 'registerpoint', 'exceptionpoint'))
        + '\n' + function(gdb, 'debug_gdbstub::cmd_reply debug_gdbstub::handle_Z('),
}

with tempfile.TemporaryDirectory(prefix='breakpoint-native-') as directory:
    work = Path(directory)
    for name, source in fragments.items():
        (work / name).write_text(source + '\n')
    for adapter in args.adapter or ('ppsspp', 'pcsx2', 'mame'):
        test = ROOT / '_tests/native' / ('breakpoint-' + adapter + '-test.cpp')
        executable = work / adapter
        subprocess.run(['c++', '-std=c++20', '-Wall', '-Wextra', '-Werror',
                        '-fsanitize=address,undefined', '-fno-sanitize-recover=all',
                        '-I' + str(work), str(test), '-o', str(executable)], check=True)
        subprocess.run([str(executable)], check=True, timeout=20)
        print('PASS native sanitizer fixture:', adapter, flush=True)
        if args.windows_objects:
            args.windows_objects.mkdir(parents=True, exist_ok=True)
            subprocess.run(['zig', 'c++', '-target', 'x86_64-windows-gnu', '-std=c++20',
                            '-Wall', '-Wextra', '-Werror', '-I' + str(work), '-c', str(test),
                            '-o', str(args.windows_objects / (adapter + '.o'))], check=True)
            print('PASS Windows x64 object:', adapter, flush=True)
