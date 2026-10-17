#!/usr/bin/env python3
"""Check patched Mesen callback dispatch and MAME console parameter boundaries.

--sources contains mesen2/ and mame-pc98/ with the maintained patches applied.
Runs extracted production methods with controlled dependencies, without a guest.
"""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def function(source, start):
    begin = source.index(start)
    brace = source.index('{', begin)
    masked = re.sub(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'',
                    lambda m: ' ' * len(m.group()), source, flags=re.S)
    depth = 0
    for end in range(brace, len(source)):
        depth += (masked[end] == '{') - (masked[end] == '}')
        if depth == 0:
            return source[begin:end + 1]
    raise ValueError('Unclosed function: ' + start)


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sources', type=Path, required=True)
parser.add_argument('--windows-objects', type=Path)
parser.add_argument('--adapter', choices=('mesen', 'mame-console'), action='append')
args = parser.parse_args()
fragments = {}
selected = args.adapter or ('mesen', 'mame-console')
if 'mesen' in selected:
    context = args.sources / 'mesen2/Core/Debugger/ScriptingContext.cpp'
    source = context.read_text()
    header = context.with_suffix('.h').read_text()
    fragments['mesen-members.inc'] = (
        header[header.index('\tstruct CallbackIteration'):header.index('\tvector<MemoryCallback>')]
        if 'struct CallbackIteration' in header else '')
    fragments['mesen-functions.inc'] = '\n'.join(function(source, start) for start in (
        'void ScriptingContext::RegisterMemoryCallback(',
        'void ScriptingContext::UnregisterMemoryCallback(',
        'void ScriptingContext::RegisterEventCallback(',
        'void ScriptingContext::UnregisterEventCallback(',
        'bool ScriptingContext::IsAddressMatch(',
        'template<typename T>\nvoid ScriptingContext::InternalCallMemoryCallback(',
        'int ScriptingContext::CallEventCallback('))
    if 'void ScriptingContext::CallbackRemoved(' in source:
        fragments['mesen-functions.inc'] += '\n' + function(source, 'void ScriptingContext::CallbackRemoved(')
if 'mame-console' in selected:
    console = args.sources / 'mame-pc98/src/emu/debug/debugcon.cpp'
    source = console.read_text()
    header = console.with_suffix('.h').read_text()
    fragments['mame-cmderr.inc'] = function(header, 'class CMDERR') + ';'
    fragments['mame-console-functions.inc'] = '\n'.join(function(source, start) for start in (
        'std::string_view debugger_console::trim_parameter(',
        'CMDERR debugger_console::internal_execute_command(',
        'CMDERR debugger_console::internal_parse_command('))
    if 'CMDERR debugger_console::execute_command_with_parameters(' in source:
        fragments['mame-console-functions.inc'] += '\n#define HAS_STRUCTURED_COMMAND 1\n' + function(
            source, 'CMDERR debugger_console::execute_command_with_parameters(')

with tempfile.TemporaryDirectory(prefix='callback-lifetime-') as directory:
    work = Path(directory)
    for name, source in fragments.items():
        (work / name).write_text(source + '\n')
    for adapter in selected:
        test = ROOT / '_tests/native' / (adapter + '-callback-test.cpp')
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
