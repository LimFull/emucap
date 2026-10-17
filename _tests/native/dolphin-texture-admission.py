#!/usr/bin/env python3
"""Exercise Dolphin's actual texture admission, serializer and decoder with a fake GPU.

--source is a complete prepared Dolphin checkout. --patched-source optionally overlays
only touched files from a pinned patch replay. No emulator or guest is launched.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys
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
parser.add_argument('--source', type=Path, required=True)
parser.add_argument('--patched-source', type=Path)
parser.add_argument('--case', choices=('all', 'entry-limit', 'late-invalid', 'legitimate',
                                      'self-reference', 'cube-budget'), default='all')
args = parser.parse_args()
base = args.source.resolve()
overlay = (args.patched_source or base).resolve()


def native(path):
    candidate = overlay / path
    return candidate if candidate.exists() else base / path


source = native('Source/Core/VideoCommon/TextureCacheBase.cpp').read_text()
parts = [function(source, 'TCacheEntry::~TCacheEntry()').replace(
             'g_texture_cache', 'g_fixture_texture_cache'),
         function(source, 'void TCacheEntry::DoState(')]
if 'struct TextureStateBudget' in source:
    parts += ['#define HAS_TEXTURE_ADMISSION 1', function(source, 'struct TextureStateBudget') + ';',
              function(source, 'bool PreflightTextureState(')]
for signature in ('void TextureCacheBase::DoLoadState(', 'void TextureCacheBase::DoSaveState(',
                  'std::optional<TextureCacheBase::TexPoolEntry> TextureCacheBase::DeserializeTexture('):
    # Only substitute dependency owners; production method bodies and field order are unchanged.
    parts.append(function(source, signature).replace('TextureCacheBase::', 'FixtureCache::')
                 .replace('Config::', 'TestConfig::'))
with tempfile.TemporaryDirectory(prefix='dolphin-texture-admission-') as directory:
    work = Path(directory)
    (work / 'texture-state.inc').write_text('\n'.join(parts) + '\n')
    executable = work / 'texture-state'
    flags = ['c++', '-std=c++23', '-O1', '-g', '-Wall', '-Wextra', '-DFMT_HEADER_ONLY',
             '-fsanitize=address,undefined', '-fno-sanitize-recover=all',
             '-ffunction-sections', '-fdata-sections', '-I' + str(work),
             '-I' + str(overlay / 'Source/Core'), '-I' + str(base / 'Source/Core'),
             '-I' + str(base / 'Externals/fmt/fmt/include'),
             '-I' + str(base / 'Externals/imgui/imgui')]
    sources = [ROOT / '_tests/native/dolphin-texture-test.cpp'] + [native(
        'Source/Core/VideoCommon/' + name + '.cpp') for name in
        ('AbstractTexture', 'AbstractFramebuffer', 'TextureConfig')]
    subprocess.run(flags + list(map(str, sources)) +
                   ['-Wl,-dead_strip' if sys.platform == 'darwin' else '-Wl,--gc-sections',
                    '-o', str(executable)], check=True)
    subprocess.run([str(executable), args.case], check=True, timeout=60)
