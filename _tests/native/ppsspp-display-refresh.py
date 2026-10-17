#!/usr/bin/env python3
"""Check native SDL refresh admission and its menu-wait bound."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, required=True)
a = p.parse_args()
s = (a.source / 'SDL/SDLMain.cpp').read_text()
start = s.index('\tg_RefreshRate = displayMode->refresh_rate;', s.index('int main(')) if '\tg_RefreshRate = displayMode->refresh_rate;' in s else s.index('\tg_RefreshRate = std::isfinite(')
assignment = s[start:s.index(';', start) + 1]
code = '''
#include <cassert>
#include <cmath>
#include <limits>
struct DisplayMode { float refresh_rate; };
float admit(float rate) {
 DisplayMode mode{rate}; auto *displayMode = &mode;
 float g_RefreshRate = 60.0f;
''' + assignment + '''
 return g_RefreshRate;
}
int main() {
 for (float r : {0.f, -1.f, 0.01f, std::numeric_limits<float>::infinity(),
                 -std::numeric_limits<float>::infinity(), std::numeric_limits<float>::quiet_NaN()}) {
  assert(admit(r) == 60.f);
 }
 for (float r : {1.f, 24.f, 59.94f, 60.f, 120.f, 240.f}) {
  assert(admit(r) == r);
  double wait = 1000000.0 / admit(r);
  assert(std::isfinite(wait) && wait > 0 && wait <= 1000000.0);
 }
}
'''
code = '#include <initializer_list>\n' + code
with tempfile.TemporaryDirectory() as directory:
 root = Path(directory)
 (root/'check.cpp').write_text(code)
 subprocess.run(['c++', '-std=c++17', str(root/'check.cpp'), '-o', str(root/'check')], check=True)
 subprocess.run([str(root/'check')], check=True)
print('SDL display refresh admission passed')
