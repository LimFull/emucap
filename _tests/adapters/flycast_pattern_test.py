"""Compile the production Flycast pattern handler against a bounded memory fixture."""
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
source = (ROOT / "adapters/flycast/emucap.cpp").read_text()
handler = source[source.index("void handle_find_pattern("):source.index("void handle_disassemble(")]
fixture = r"""
#include <string>
#include <vector>
#include <cstdint>
#include <cstdlib>
#include <cstdio>
#include <cassert>
#include "emucap_pattern.h"
using u8 = uint8_t;
using u32 = uint32_t;
struct DCRegion { const char* mt; u32 base, size; };
static DCRegion region{"ram", 0, 8192};
static std::vector<u8> memory(8192, 0);
static std::string pattern, response, error;
static long limit = 256, alignment = 1, start_at = 0;
const DCRegion* find_region(const std::string&) { return &region; }
std::string json_str(const std::string&, const char* key) {
    return std::string(key) == "hex" ? pattern : "ram";
}
bool json_num(const std::string&, const char* key, long& value) {
    std::string name(key);
    if (name == "max_matches") value = limit;
    else if (name == "align") value = alignment;
    else if (name == "start") value = start_at;
    else return false;
    return true;
}
void reply_err(long, const char*, const char* message) { error = message; }
void reply_ok(long, const std::string& value) { response = value; }
void reply_unsupported_memtype(long, const std::string&) { assert(false); }
namespace addrspace { u8 read8(u32 address) { return memory.at(address); } }
"""
checks = r"""
int main() {
    pattern = "00"; limit = 2147483647;
    handle_find_pattern(1, "");
    assert(response.find("\"count\":4096") != std::string::npos);
    assert(response.find("\"truncated\":true") != std::string::npos);
    pattern = "0g"; response.clear();
    handle_find_pattern(2, "");
    assert(!error.empty() && response.empty());
    error.clear(); pattern = "0000"; limit = 2; alignment = 2; start_at = 1;
    handle_find_pattern(3, "");
    assert(error.empty());
    assert(response.find("\"matches\":[2,4]") != std::string::npos);
    region.size = 4; alignment = 1; start_at = 0; limit = 3;
    handle_find_pattern(4, "");
    assert(response.find("\"matches\":[0,1,2]") != std::string::npos);
    assert(response.find("\"truncated\":false") != std::string::npos);
    limit = 2;
    handle_find_pattern(5, "");
    assert(response.find("\"truncated\":true") != std::string::npos);
}
"""
with tempfile.TemporaryDirectory(prefix="emucap-pattern-test-") as directory:
    path = Path(directory)
    (path / "test.cpp").write_text(fixture + handler + checks)
    subprocess.run([
        os.environ.get("CXX", "c++"), "-std=c++11", "-Wall", "-Wextra", "-Werror",
        "-fsanitize=address,undefined", "-I" + str(ROOT / "adapters/_common"),
        str(path / "test.cpp"), "-o", str(path / "test"),
    ], check=True)
    subprocess.run([str(path / "test")], check=True)
print("Flycast production pattern handler passed")
