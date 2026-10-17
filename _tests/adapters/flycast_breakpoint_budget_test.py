"""Exercise production breakpoint admission, clear, alias and list handlers."""
from pathlib import Path
import os
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / 'adapters/flycast/emucap.cpp').read_text()


def function(signature):
    start = source.index(signature)
    return source[start:source.index('\n}', start) + 2]


fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <limits>
#include <set>
#include <string>
#include <vector>
struct EmuBp { long id; uint32_t addr; };
std::vector<EmuBp> g_bps;
std::set<uint32_t> g_bp_addrs;
bool g_emucap_bp_armed = false;
long g_bp_next_id = 1, address = 0x0c001000, clear_id = 1;
std::string response, error;
uint32_t sh4_fold_pc(uint32_t pc) { return pc < 0xe0000000 ? pc & 0x1fffffff : pc; }
std::string json_str(const std::string&, const char*) { return "exec"; }
bool json_num(const std::string&, const char*, long& out) { out = address; return true; }
bool json_num_from(const std::string&, const char*, long& out, size_t) { out = clear_id; return true; }
void reply_ok(long, const std::string& out) { response = out; }
void reply_err(long, const char*, const char* out) { error = out; }
'''
handlers = '\n'.join(function(name) for name in (
    'void rearm_breakpoints(', 'void handle_set_breakpoint(',
    'void handle_clear_breakpoint(', 'void handle_list_breakpoints('))
checks = r'''
int main() {
    for (long i = 1; i <= 128; ++i) {
        address = i % 2 ? 0x0c001000 : 0x2c001000;
        handle_set_breakpoint(i, "");
        assert(error.empty() && g_bps.size() == size_t(i) && g_bps.back().id == i);
    }
    assert(g_bp_addrs.size() == 1 && g_emucap_bp_armed);
    handle_set_breakpoint(129, "");
    assert(!error.empty() && g_bps.size() == 128 && g_bp_next_id == 129);
    handle_list_breakpoints(130);
    assert(response.find("\"id\":128,") != std::string::npos && response.size() < 16384);
    clear_id = 64;
    handle_clear_breakpoint(131, "\"id\":131,\"id\":64");
    assert(g_bps.size() == 127 && g_bp_addrs.size() == 1);
    error.clear(); address = 0x0c002000;
    handle_set_breakpoint(132, "");
    assert(error.empty() && g_bps.back().id == 129 && g_bp_addrs.size() == 2);
    for (clear_id = 1; clear_id <= 129; ++clear_id)
        handle_clear_breakpoint(133, "\"id\":133,\"id\":0");
    assert(g_bps.empty() && g_bp_addrs.empty() && !g_emucap_bp_armed);
    g_bp_next_id = std::numeric_limits<long>::max() - 1;
    handle_set_breakpoint(134, "");
    assert(error.empty() && g_bps.size() == 1);
    handle_list_breakpoints(135);
    assert(response.find(std::to_string(g_bp_next_id - 1)) != std::string::npos);
    handle_set_breakpoint(136, "");
    assert(!error.empty() && g_bps.size() == 1);
}
'''
with tempfile.TemporaryDirectory(prefix='flycast-breakpoints-') as directory:
    path = Path(directory)
    (path / 'test.cpp').write_text(fixture + handlers + checks)
    subprocess.run([os.environ.get('CXX', 'clang++'), '-std=c++17', '-Wall', '-Wextra',
                    '-Werror', '-fsanitize=address,undefined', str(path / 'test.cpp'),
                    '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True, timeout=20)
print('Flycast breakpoint capacity, duplicates, clear recovery and ID exhaustion passed')
