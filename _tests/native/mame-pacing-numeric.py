#!/usr/bin/env python3
"""Check shipped native pacing conversions with actual attotime normalization bodies.

This is a component test; full emulator/platform qualification is separate.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=root / 'adapters/mame-pc98/work/mame-src/src/emu')
source = parser.parse_args().source
cpp = (source / 'video.cpp').read_text()
header = (source / 'video.h').read_text()
time = (source / 'attotime.h').read_text()


def body(text, signature):
    start = text.index(signature)
    end = text.index('{', start) + 1
    depth = 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


setup = r'''
#include <cassert>
#include <cmath>
#include <cstdint>
#include <limits>
#include <stdexcept>
#include <algorithm>
#include <initializer_list>
using attoseconds_t=int64_t;using seconds_t=int32_t;using osd_ticks_t=uint64_t;
constexpr attoseconds_t ATTOSECONDS_PER_SECOND=1000000000000000000LL;
constexpr seconds_t ATTOTIME_MAX_SECONDS=1000000000;
struct attotime {
 seconds_t m_seconds=0;attoseconds_t m_attoseconds=0;
 attotime()=default;
 ACTUAL_CONSTRUCTOR
 static const attotime never;
 static attotime from_double(double);
 attotime &operator+=(const attotime&) noexcept;
};
const attotime attotime::never{ATTOTIME_MAX_SECONDS,0};
'''.replace('ACTUAL_CONSTRUCTOR', body(time, 'constexpr attotime(seconds_t secs,'))
setup += '\n'.join(body(time, sig) for sig in [
    'inline constexpr attoseconds_t DOUBLE_TO_ATTOSECONDS(',
    'inline attotime attotime::from_double(',
    'inline attotime &attotime::operator+=(',
])
setup += '\n'.join(body(cpp, sig) for sig in [
    'static attoseconds_t checked_throttle_period(', 'static bool throttle_elapsed(',
])
setup += r'''
static osd_ticks_t frequency=1000000000;
static osd_ticks_t osd_ticks_per_second(){return frequency;}
struct video_manager {
 float m_throttle_rate=1;bool m_throttle_resync=false;uint32_t m_speed=1000;
 bool is_valid_throttle_rate(double) const;
'''
setup += '\n'.join(body(header, 'void set_' + name + '(') for name in ['throttle_rate', 'speed_factor'])
setup += '\n};\n' + body(cpp, 'bool video_manager::is_valid_throttle_rate(')
checks = r'''
int main(){
 video_manager video;
 for(double bad: {0.0,-0.0,-1.0,1e-100,1e-30,1e9,1e100,
     std::numeric_limits<double>::infinity(),std::numeric_limits<double>::quiet_NaN()}) {
  assert(!video.is_valid_throttle_rate(bad));
 }
 for(float bad: {0.0f,-1.0f,1e-30f,1e9f,
     std::numeric_limits<float>::infinity(),std::numeric_limits<float>::quiet_NaN()}) {
  bool threw=false;try { video.set_throttle_rate(bad); } catch(const std::invalid_argument&) { threw=true; }
  assert(threw && video.m_throttle_rate==1 && !video.m_throttle_resync);
 }
 bool threw=false;try { video.set_speed_factor(-1); } catch(const std::invalid_argument&) { threw=true; }
 assert(threw && video.m_speed==1000 && !video.m_throttle_resync);
 for(int speed: {0,1,100000,std::numeric_limits<int>::max()}) {video.set_speed_factor(speed);assert(video.m_speed==uint32_t(speed));}
 for(auto hz: {1ULL,1000000ULL,10000000ULL,1000000000ULL,1000000000000ULL,1000000000000000000ULL}) {
  frequency=hz;
  const auto base=ATTOSECONDS_PER_SECOND/frequency;
  float minimum=float(1.0/double(base));
  while(!video.is_valid_throttle_rate(minimum)) minimum=std::nextafter(minimum,INFINITY);
  assert(!video.is_valid_throttle_rate(std::nextafter(minimum,0.0f)));
  float maximum=std::min(float(ATTOTIME_MAX_SECONDS),float(double(std::numeric_limits<attoseconds_t>::max())/double(base)));
  while(!video.is_valid_throttle_rate(maximum)) maximum=std::nextafter(maximum,0.0f);
  assert(!video.is_valid_throttle_rate(std::nextafter(maximum,INFINITY)));
  for(float rate: {minimum,maximum,0.75f,1.0f,2.0f}) {
   if(!video.is_valid_throttle_rate(rate)) continue;
   video.set_throttle_rate(rate);video.m_throttle_resync=false;
   video.set_throttle_rate(rate);assert(!video.m_throttle_resync);
   const auto period=checked_throttle_period(rate,frequency);assert(period>0);
   for(auto ticks: {uint64_t(0),uint64_t(1),frequency/2,frequency-1,std::numeric_limits<uint64_t>::max()}) {
    attotime elapsed(42,0);
    const long double wanted=(long double)ticks*period/ATTOSECONDS_PER_SECOND;
    if(!throttle_elapsed(ticks,period,elapsed)) {assert(wanted>=ATTOTIME_MAX_SECONDS-1);assert(elapsed.m_seconds==42);continue;}
    assert(elapsed.m_seconds>=0 && elapsed.m_seconds<ATTOTIME_MAX_SECONDS);
    assert(elapsed.m_attoseconds>=0 && elapsed.m_attoseconds<ATTOSECONDS_PER_SECOND);
    const long double actual=elapsed.m_seconds+(long double)elapsed.m_attoseconds/ATTOSECONDS_PER_SECOND;
    assert(fabsl(actual-wanted)<1e-6L);
    attotime sum(100,ATTOSECONDS_PER_SECOND-1);sum+=elapsed;
    assert(sum.m_seconds<=ATTOTIME_MAX_SECONDS);
    assert(sum.m_attoseconds>=0 && sum.m_attoseconds<ATTOSECONDS_PER_SECOND);
   }
  }
 }
 assert(checked_throttle_period(1,0)==0);
 assert(checked_throttle_period(1,ATTOSECONDS_PER_SECOND+1)==0);
 // The previous integer product overflowed, and attotime(0,product) was not normalized.
 attotime elapsed;assert(throttle_elapsed(999999999,8000000000LL,elapsed));
 assert(elapsed.m_seconds==7 && elapsed.m_attoseconds>0);
}
'''
with tempfile.TemporaryDirectory(prefix='mame-pacing-numeric-') as temp:
    temp = Path(temp)
    (temp / 'test.cpp').write_text(setup + checks)
    subprocess.run(['c++', '-std=c++20', '-O1', '-g', '-fsanitize=address,undefined,float-cast-overflow',
                    '-fno-sanitize-recover=all', '-Wall', '-Wextra', '-Werror', '-Wno-sign-compare',
                    str(temp / 'test.cpp'), '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
print('native pacing numeric: float bounds, native setters, exact rate edges, normalized durations and overflow guards passed')
