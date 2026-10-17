#!/usr/bin/env python3
"""Execute the maintained controller command dispatch and producer poll hook."""
import argparse, os, subprocess, tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True)
a=p.parse_args()
s=(a.source/'backends/plugins_compat/input_plugin_compat.c').read_text()
hook=s[s.index('/* Registered before execution;'):s.index('static int is_button_released(')]
s=(a.source/'device/controllers/game_controller.c').read_text()
dispatch=s[s.index('static void process_controller_command('):s.index('const struct joybus_device_interface g_ijoybus_device_controller')]
program=r'''
#include <assert.h>
#include <string.h>
#include "api/m64p_types.h"
#include "device/controllers/game_controller.h"
#include "backends/api/controller_input_backend.h"
#include "backends/plugins_compat/plugins_compat.h"
static int input_result, count;
static uint32_t response[16];
static void DebugMessage(int level,const char* format,...) {}
static void pak_read_block(struct game_controller* c,const uint8_t* a,uint8_t* b,uint8_t* d) {}
static void pak_write_block(struct game_controller* c,const uint8_t* a,const uint8_t* b,uint8_t* d) {}
static m64p_error get_input(void* c,uint32_t* word) {*word=0xa6250080;return input_result;}
const struct controller_input_backend_interface g_icontroller_input_backend_plugin_compat={get_input};
'''+hook+dispatch+r'''
static void observed(uint32_t word) {assert(response[0]==word);assert(word==0xa6250080);++count;}
static void reset_controller(struct game_controller* c) {}
int main(void) {
 struct controller_input_compat owner; memset(&owner,0,sizeof(owner));
 struct game_controller_flavor flavor={"fixture",JDT_JOY_ABS_COUNTERS,reset_controller};
 struct game_controller cont={0};cont.cin=&owner;cont.icin=&g_icontroller_input_backend_plugin_compat;cont.flavor=&flavor;
 assert(CoreEmucapSetPollCallback(2,observed)==M64ERR_INPUT_INVALID);
 assert(CoreEmucapSetPollCallback(1,observed)==M64ERR_SUCCESS);
 uint8_t commands[][3]={{JCMD_STATUS,1,3},{JCMD_RESET,1,3},{JCMD_PAK_READ,3,33},{JCMD_PAK_WRITE,35,1},{JCMD_CONTROLLER_READ,1,3},{JCMD_CONTROLLER_READ,2,4}};
 for(unsigned i=0;i<sizeof(commands)/sizeof(commands[0]);++i) {
  uint8_t data[40]={commands[i][0]},tx=commands[i][1],rx=commands[i][2];
  process_controller_command(&cont,&tx,data,&rx,(uint8_t*)response);assert(count==0);
 }
 uint8_t tx=1,rx=4,data[40]={JCMD_CONTROLLER_READ};
 input_result=M64ERR_INPUT_INVALID;process_controller_command(&cont,&tx,data,&rx,(uint8_t*)response);assert(count==0 && (rx&0x80));
 input_result=M64ERR_SUCCESS;rx=4;owner.control_id=1;
 process_controller_command(&cont,&tx,data,&rx,(uint8_t*)response);assert(count==0);
 struct controller_input_backend_interface other={get_input};cont.icin=&other;owner.control_id=0;
 process_controller_command(&cont,&tx,data,&rx,(uint8_t*)response);assert(count==0);
 cont.icin=&g_icontroller_input_backend_plugin_compat;
 for(int i=0;i<5;i++){process_controller_command(&cont,&tx,data,&rx,(uint8_t*)response);assert(count==i+1);}
 CoreEmucapSetPollCallback(1,0);process_controller_command(&cont,&tx,data,&rx,(uint8_t*)response);assert(count==5);
 return 0;
}
'''
with tempfile.TemporaryDirectory() as d:
 src=Path(d)/'poll.c';src.write_text(program);binary=Path(d)/'poll'
 subprocess.run([os.environ.get('CC','cc'),'-fsanitize=address,undefined','-g','-I',str(a.source),str(src),'-o',str(binary)],check=True)
 subprocess.run([str(binary)],check=True,timeout=10)
print('successful port-0 replies only, response-before-hook, exact raw word: passed')
