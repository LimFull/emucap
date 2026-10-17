#!/usr/bin/env python3
"""Exercise the maintained raw-input owner with native SDL synchronization."""
import argparse, os, shlex, subprocess, tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True)
a=p.parse_args()
source=(a.source/'backends/plugins_compat/input_plugin_compat.c').read_text()
owner=source[source.index('static SDL_SpinLock emucap_input_lock;'):source.index('/* Registered before execution;')]
program='''#include <SDL.h>
#include <stdint.h>
#include <assert.h>
#include <pthread.h>
#define EXPORT
#define CALL
typedef int m64p_error;
#define M64ERR_INPUT_INVALID 1
#define M64ERR_SUCCESS 0
'''+owner+r'''
static void* writer(void* unused) {
 for (int i=0;i<100000;i++) assert(CoreEmucapInput(1,0,1,i%2?0xa0:0x1010,i%2?37:-128,i%2?-90:127)==0);
 return 0;
}
int main(void) {
 uint32_t v=0xdeadbeef;
 emucap_controller_input(0,&v); assert(v==0xdeadbeef);
 for(int x=-128;x<=127;x++) for(int y=-128;y<=127;y++) {
  assert(CoreEmucapInput(1,0,1,0xa0,x,y)==0);
  v=0xffffffff;emucap_controller_input(0,&v);
  assert((v&0xffff)==0xa0 && ((v>>16)&255)==(uint8_t)x && (v>>24)==(uint8_t)y);
 }
 assert(CoreEmucapInput(1,0,1,0xa0,37,-90)==0);
 uint32_t good=0xa62500a0;
 for(int fault=0;fault<6;fault++) {
  assert(CoreEmucapInput(fault==0?2:1,fault==1?1:0,fault==2?2:1,fault==3?0x4000:0xa0,fault==4?128:37,fault==5?-129:-90)!=0);
  v=0;emucap_controller_input(0,&v);assert(v==good);
 }
 pthread_t thread;assert(pthread_create(&thread,0,writer,0)==0);
 for(int i=0;i<100000;i++){v=0;emucap_controller_input(0,&v);assert(v==good || v==0x7f801010);}
 pthread_join(thread,0);
 assert(CoreEmucapInput(1,0,1,0,0,0)==0);v=0xffffffff;emucap_controller_input(0,&v);assert(v==0);
 assert(CoreEmucapInput(1,0,0,0,0,0)==0);v=0x12345678;emucap_controller_input(0,&v);assert(v==0x12345678);
 return 0;
}
'''
flags=shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','sdl2'],text=True))
with tempfile.TemporaryDirectory() as d:
 src=Path(d)/'input.c';src.write_text(program);out=Path(d)/'test'
 subprocess.run([os.environ.get('CC','cc'),'-fsanitize=address,undefined','-g',str(src),'-o',str(out),'-pthread',*flags],check=True)
 env=os.environ.copy()
 libdir=subprocess.check_output(['pkg-config','--variable=libdir','sdl2'],text=True).strip()
 env['DYLD_LIBRARY_PATH']=libdir+(':'+env['DYLD_LIBRARY_PATH'] if env.get('DYLD_LIBRARY_PATH') else '')
 subprocess.run([str(out)],check=True,env=env,timeout=30)
print('native raw bytes, rejection, concurrent coherent publication and release: passed')
