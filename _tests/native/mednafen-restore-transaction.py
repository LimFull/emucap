#!/usr/bin/env python3
"""Exercise the production restore transaction and probe routing with fault injection."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
source = (root / "adapters/mednafen/emucap.cpp").read_text()
transaction = source[source.index("bool restore_state_transaction("):source.index("void handle_load_state(")]
probe = source.split('} else if (method == "probe") {', 1)[1].split('} else if (method == "set_breakpoint")', 1)[0]
code = r'''
#include <array>
#include <vector>
#include <string>
#include <memory>
#include <stdexcept>
#include <cassert>
#include <cstring>
#include "emucap_native_control.h"
using uint32 = uint32_t;
using Bytes = std::vector<uint8_t>;
static int fault, loads, allocations, reads, prepared, commits;
static bool quarantined, history;
static uint64_t file_size = 32;
static std::string error, path = "state";
struct FileStream {
 enum { MODE_READ };
 FileStream(const std::string&, int) {}
 uint64_t size() { return file_size; }
 void read(void* p, size_t n) { memset(p, 0, n); }
 void close() {}
};
struct MemoryStream {
 Bytes bytes;
 MemoryStream(size_t n, int):bytes(n) { ++allocations; }
 uint8_t* map() { return bytes.data(); }
 size_t map_size() { return bytes.size(); }
 void truncate(size_t n) { bytes.resize(n); }
};
struct EmucapSnapshot {
 static constexpr uint64_t max_bytes = 512ull*1024*1024;
 size_t native_size = 32;
 bool history;
 std::array<Bytes, 4> blocks;
 static int identity(const char*, const uint8_t*) { return 0; }
 static EmucapSnapshot decode(const uint8_t*, size_t, int) {
  if (fault == 1) throw std::runtime_error("invalid state");
  EmucapSnapshot s; s.history = ::history; return s;
 }
};
struct Game { const char* shortname = "pce"; uint8_t MD5[16]{}; int soundchan = 2; } game;
static Game* MDFNGameInfo = &game;
void MDFNSS_LoadSM(MemoryStream*) {
 ++loads;
 if (fault == 2) throw std::runtime_error("native mutation failed");
 if (fault == 3) throw 1;
 if (history) assert(prepared == 2);
}
int emucap_capture_live_input() { return 7; }
bool emucap_restore_live_input(int value) { assert(value == 7); return fault != 4; }
struct EmucapDriverVideo {};
struct EmucapVideoBlocks { Bytes a,b,c; };
static int g_completed_frame;
struct PreparedVideo {
 bool commit(EmucapDriverVideo&, int&) { ++commits; return fault != 6; }
};
struct EmucapVideoRestore {
 static std::unique_ptr<PreparedVideo> prepare(EmucapVideoBlocks&, EmucapDriverVideo&) {
  ++prepared; if (fault == 5) throw std::runtime_error("prepare failed");
  return std::unique_ptr<PreparedVideo>(new PreparedVideo);
 }
};
struct EmucapFrameOutput { static EmucapFrameOutput decode(const Bytes&) { return {}; } };
namespace EmucapFrameCall { using Output = std::unique_ptr<EmucapFrameOutput>; }
int emucap_frame_binding(bool) { return 0; }
int emucap_prepare_frame_output(EmucapFrameCall::Output, int, int) { ++prepared; return 0; }
bool emucap_commit_frame_output(int, int) { ++commits; return fault != 7; }
void quarantine_state_restore(const char*) { quarantined = true; }
void reply_err(long, const char* kind, const char*) { error = kind; }
void reply_ok(long, const std::string&) {}
EmucapControl::NativeControl g_native_control;
std::string json_str(const std::string&, const char* key) { return std::string(key) == "state" ? path : "ram"; }
static long frame_count;
void json_num(const std::string&, const char* key, long& v) { v = std::string(key) == "frame" ? frame_count : 1; }
bool json_u32_arg(long, const std::string&, const char*, uint32& v, bool) { v=0; return true; }
bool normalize_sync_advance(long, long, bool) { return true; }
bool reject_ss_physical_read(long, const std::string&) { return false; }
bool validate_aspace_range(const std::string&, uint32, long) { return true; }
bool read_aspace_hex(const std::string&, uint32, long, std::string&) { ++reads; return true; }
static long g_probe_id=-1, g_probe_requested, g_probe_remaining, g_probe_progress_ms, g_probe_len;
static uint32 g_probe_addr;
static std::string g_probe_mt;
long monotonic_millis() { return 1; }
void start_advance_clock() {}
'''
checks = r'''
void reset() {
 fault=loads=allocations=reads=prepared=commits=0;
 quarantined=history=false; error.clear(); file_size=32; g_probe_id=-1;
}
int main() {
 bool restored=false;
 for (bool wrapped : {false,true}) {
  reset(); history=wrapped;
  assert(restore_state_transaction(1,"state",restored));
  assert(loads==1 && restored==wrapped && !quarantined);
 }
 for (int f=1; f<=7; ++f) {
  reset(); history=true; fault=f;
  assert(!restore_state_transaction(1,"state",restored));
  const bool mutated=f!=1 && f!=5;
  assert(loads==int(mutated) && quarantined==mutated);
  assert(error==(mutated ? "restore_unverified" : "io_error"));
 }
 for (auto n : {uint64_t(31), EmucapSnapshot::max_bytes+1}) {
  reset(); file_size=n;
  assert(!restore_state_transaction(1,"state",restored));
  assert(allocations==0 && loads==0 && !quarantined);
 }
 using C=EmucapControl::NativeControl::Context;
 for (auto context : {C::None,C::Cpu,C::Continuation,C::Device,C::Idle,C::Pacing,C::Frame}) {
  reset();
  EmucapControl::NativeControl::Scope scope(g_native_control,context);
  probe(1,"");
  assert(loads==int(context==C::Frame) && reads==int(context==C::Frame));
 }
 for (long frames : {0L,3L}) {
  reset(); fault=2; frame_count=frames;
  EmucapControl::NativeControl::Scope scope(g_native_control,C::Frame);
  probe(1,"");
  assert(quarantined && reads==0 && g_probe_id==-1);
 }
 reset(); frame_count=3;
 {
  EmucapControl::NativeControl::Scope scope(g_native_control,C::Frame);
  probe(9, "");
  assert(g_probe_id==9 && g_probe_requested==3 && g_probe_remaining==3);
  assert(g_probe_progress_ms==1 && g_probe_addr==0 && g_probe_len==1);
 }
 reset(); g_native_control.Invalidate();
 EmucapControl::NativeControl::Scope scope(g_native_control,C::Frame);
 probe(1,""); assert(loads==0 && error=="unsafe_halt");
}
'''
with tempfile.TemporaryDirectory(prefix="mednafen-restore-") as temporary:
    directory = Path(temporary)
    cpp = directory / "test.cpp"
    cpp.write_text(code + transaction + '\nvoid probe(long id,const std::string& line) {\n' + probe + '\n}\n' + checks)
    binary = directory / "test"
    subprocess.run(["clang++", "-std=c++11", "-Wall", "-Wextra", "-Werror",
        "-fsanitize=address,undefined", "-I" + str(root / "adapters/mednafen"),
        str(cpp), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print("Restore transaction and probe context/failure routing passed")
