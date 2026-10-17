#!/usr/bin/env python3
"""Exercise the maintained Dolphin event queue and poll boundary without launching a guest."""
from pathlib import Path
import argparse
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=root / 'adapters/dolphin/EmuCap.cpp')
parser.add_argument('--windows-object', type=Path)
args = parser.parse_args()
source = args.source.read_text()
globals_ = source[source.index('std::mutex s_ev_mutex;'):source.index('std::mutex s_bp_mutex;')]
if 's_event_bytes' in globals_:
    globals_ += '\n#define EMUCAP_TEST_EVENT_COUNTER 1\n'
start = source.index('// Retained event admission') if '// Retained event admission' in source else source.index('void PushEvent(')
push = source[start:source.index('// Request handlers return')]
poll = source[source.index('picojson::object PollEvents('):source.index('picojson::object SaveState(')]
breakpoints = source[source.index('std::mutex s_bp_mutex;'):source.index('int s_next_bp =')]
notifications = source[source.index('void SignalBreakpointInterruption()'):source.index('void NotifyFrameStepComplete()')]
code = r'''
#include <picojson.h>
#include <cassert>
#include <condition_variable>
#include <cstdio>
#include <cstdint>
#include <deque>
#include <iterator>
#include <map>
#include <mutex>
#include <thread>
#include <vector>
using u32 = uint32_t;
using u64 = uint64_t;
namespace Core { struct System { int GetPPCState() { return 0; } }; }
size_t capture_size = 0;
picojson::object CpuState(int) {
  return {{"pc", picojson::value(123.0)}, {"capture", picojson::value(std::string(capture_size, 'x'))}};
}
std::mutex s_frame_step_mutex;
std::condition_variable s_frame_step_cv, s_reset_cv;
u64 s_breakpoint_interruptions = 0;
''' + globals_ + push + poll + breakpoints + r'''
const char* BreakpointKindName(BreakpointKind kind) {
  switch (kind) {
  case BreakpointKind::Exec: return "exec";
  case BreakpointKind::Read: return "read";
  case BreakpointKind::Write: return "write";
  }
  return "invalid";
}
''' + notifications + r'''
int main() {
  Core::System system;
  for (int i = 0; i < 257; ++i) PushEvent(picojson::value(double(i)));
  auto result = PollEvents(system, {});
  auto events = result["events"].get<picojson::array>();
  assert(events.size() == 256 && result["dropped"].get<double>() == 1);
  assert(events.front().get<double>() == 1 && events.back().get<double>() == 256);
  result = PollEvents(system, {});
  assert(result["events"].get<picojson::array>().empty() && result["dropped"].get<double>() == 0);

  // Escaped control characters count six serialized bytes each, not one source byte.
  for (int i = 0; i < 3; ++i) PushEvent(picojson::value(std::string(100000, '\1')));
  result = PollEvents(system, {});
  assert(result["events"].get<picojson::array>().size() == 1 && result["dropped"].get<double>() == 2);
  const std::string exact(1024 * 1024 - 2, 'x');
  PushEvent(picojson::value(exact));
  PushEvent(picojson::value(exact + 'x')); // One oversized event cannot displace valid evidence.
  result = PollEvents(system, {});
  assert(result["events"].get<picojson::array>().size() == 1 && result["dropped"].get<double>() == 1);
  assert(result["events"].get<picojson::array>()[0].get<std::string>() == exact);
  assert(picojson::value(result).serialize().size() < 8 * 1024 * 1024);

  std::vector<std::thread> writers;
  for (int t = 0; t < 4; ++t)
    writers.emplace_back([] { for (int i = 0; i < 1000; ++i) PushEvent(picojson::value(double(i))); });
  for (auto& writer : writers) writer.join();
  result = PollEvents(system, {});
  assert(result["events"].get<picojson::array>().size() == 256 && result["dropped"].get<double>() == 3744);

  s_breakpoints.emplace(1, PublicBreakpoint{BreakpointKind::Exec, 100, 100});
  s_breakpoints.emplace(2, PublicBreakpoint{BreakpointKind::Read, 200, 207});
  s_breakpoints.emplace(3, PublicBreakpoint{BreakpointKind::Write, 300, 307});
  for (int i = 0; i < 257; ++i) NotifyBreakpointHit(system, 100, 1);
  assert(s_breakpoint_interruptions == 257);
  result = PollEvents(system, {});
  assert(result["events"].get<picojson::array>().size() == 256 && result["dropped"].get<double>() == 1);
  NotifyMemoryBreakpointHit(system, 0x42, 200, false, 1, 101, 2);
  NotifyMemoryBreakpointHit(system, 0x43, 300, true, 1, 102, 3);
  capture_size = 1024 * 1024;
  NotifyBreakpointHit(system, 100, 1); // Even rejected evidence must signal the native interruption.
  assert(s_breakpoint_interruptions == 260);
  result = PollEvents(system, {});
  events = result["events"].get<picojson::array>();
  assert(events.size() == 2 && result["dropped"].get<double>() == 1);
  assert(events[0].get<picojson::object>().at("access_phase").get<std::string>() == "after_read_before_instruction_complete");
  assert(events[1].get<picojson::object>().at("value_hex").get<std::string>() == "43");

#ifdef EMUCAP_TEST_EVENT_COUNTER
  s_dropped_events = MAX_REPORTED_EVENT_DROPS - 1;
  PushEvent(picojson::value(exact + 'x'));
  PushEvent(picojson::value(exact + 'x'));
  result = PollEvents(system, {});
  assert(result["dropped"].get<double>() == double(MAX_REPORTED_EVENT_DROPS));
  assert(s_event_bytes == 0 && s_events.empty() && s_dropped_events == 0);
#endif
}
'''
with tempfile.TemporaryDirectory(prefix='dolphin-events-') as directory:
    work = Path(directory)
    (work / 'test.cpp').write_text(code)
    subprocess.run(['c++', '-std=c++20', '-pthread', '-Wall', '-Wextra', '-Werror', '-fno-exceptions',
                    '-fsanitize=address,undefined', '-fno-sanitize-recover=all',
                    '-I' + str(root / 'adapters/dolphin/work/dolphin-src/Externals/picojson'),
                    str(work / 'test.cpp'), '-o', str(work / 'test')], check=True)
    subprocess.run([str(work / 'test')], check=True, timeout=30)
    if args.windows_object:
        subprocess.run(['zig', 'c++', '-target', 'x86_64-windows-gnu', '-std=c++20',
                        '-Wall', '-Wextra', '-Werror', '-fno-exceptions',
                        '-I' + str(root / 'adapters/dolphin/work/dolphin-src/Externals/picojson'),
                        '-c', str(work / 'test.cpp'), '-o', str(args.windows_object)], check=True)
        print('Dolphin event queue and native notification Windows x64 compilation passed')
print('Dolphin event byte/count limits, FIFO/loss, concurrent producers and native interruptions passed')
