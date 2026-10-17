#!/usr/bin/env python3
"""Exercise MAME's actual open plan and retry loop against filesystem failures."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True)
args = parser.parse_args()
source = (args.source / 'src/emu/diimage.cpp').read_text()
start = source.index('std::vector<u32> device_image_interface::determine_open_plan(')
plan = source[start:source.index('\n}', start) + 2]
start = source.index('\t\t// determine open plan', source.index('device_image_interface::load_internal('))
loop = source[start:source.index('\n\t\t// did we fail to find the file?', start)]
code = r'''
#include <cassert>
#include <cstdint>
#include <string>
#include <string_view>
#include <system_error>
#include <utility>
#include <vector>
using u32 = uint32_t;
constexpr u32 OPEN_FLAG_READ=1, OPEN_FLAG_WRITE=2, OPEN_FLAG_CREATE=4;
struct device_image_interface {
 bool readable=true, writable=true, creatable=true, m_file=false, readonly=false;
 std::error_condition write_error, read_error;
 std::vector<u32> attempted;
 bool is_readable() const { return readable; }
 bool is_writeable() const { return writable; }
 bool is_creatable() const { return creatable; }
 std::vector<u32> determine_open_plan(bool is_create);
 std::error_condition load_image_by_path(u32 flags, std::string_view) {
  attempted.push_back(flags);
  auto error = (flags & OPEN_FLAG_WRITE) ? write_error : read_error;
  if (!error) { m_file=true; readonly=!(flags & OPEN_FLAG_WRITE); }
  return error;
 }
 std::error_condition open(bool is_create=false) {
  std::pair<std::error_condition,std::string> err;
  std::string_view path="fixture.hdm";
''' + loop + r'''
  return err.first;
 }
};
''' + plan + r'''
int main() {
 for (auto failure : {std::errc::read_only_file_system, std::errc::permission_denied}) {
  device_image_interface d; d.write_error=failure;
  assert(!d.open() && d.m_file && d.readonly);
  assert((d.attempted==std::vector<u32>{3,1}));
 }
 device_image_interface writable;
 assert(!writable.open() && !writable.readonly && writable.attempted.size()==1);
 device_image_interface denied; denied.write_error=std::errc::read_only_file_system;
 denied.read_error=std::errc::permission_denied;
 assert(denied.open()==std::errc::permission_denied && !denied.m_file);
 device_image_interface io; io.write_error=std::errc::io_error;
 assert(io.open()==std::errc::io_error && io.attempted.size()==1 && !io.m_file);
 device_image_interface create; create.write_error=std::errc::read_only_file_system;
 assert(create.open(true)==std::errc::read_only_file_system && !create.m_file);
 assert((create.attempted==std::vector<u32>{7}));
 device_image_interface write_only; write_only.readable=false;
 write_only.write_error=std::errc::read_only_file_system;
 assert(write_only.open()==std::errc::read_only_file_system && !write_only.m_file);
 assert((write_only.attempted==std::vector<u32>{2}));
}
'''
with tempfile.TemporaryDirectory(prefix='mame-media-open-') as temporary:
    root = Path(temporary)
    cpp, binary = root / 'test.cpp', root / 'test'
    cpp.write_text(code)
    subprocess.run(['c++', '-std=c++20', '-Wall', '-Wextra', '-Werror',
                    str(cpp), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('Native media open: read-only fallback, writable fast path and terminal failures pass')
