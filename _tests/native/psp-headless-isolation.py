#!/usr/bin/env python3
"""Managed headless preserves guest dialogs and writes only its selected memstick."""
import argparse
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=root / 'adapters/ppsspp/work/ppsspp')
args = parser.parse_args()
text = (args.source / 'headless/Headless.cpp').read_text()
atlas = (args.source / 'Core/Util/PPGeDraw.cpp').read_text()
def function(signature, text=text):
    start = text.index(signature)
    opening = text.index('{', start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]
code = r'''
#include <cassert>
#include <string>
#include <vector>
#define PPSSPP_PLATFORM(x) PPSSPP_PLATFORM_##x
#define PPSSPP_PLATFORM_ANDROID 0
struct Path {
 std::string value;
 Path()=default;Path(const char* s):value(s){}Path(std::string s):value(s){}
 Path operator/(const char* suffix)const{return value+"/"+suffix;}
};
struct {Path memStickDirectory;}g_Config;
std::vector<std::string> writes;
namespace File {
 void CreateDir(const Path& p,bool){writes.push_back(p.value);}
 void CreateFullPath(const Path& p){writes.push_back(p.value);}
}
void CreateSysDirectories(){writes.push_back(g_Config.memStickDirectory.value+"/PSP/SYSTEM");}
const char* selected=nullptr;
const char* test_getenv(const char* name){return std::string(name)=="EMUCAP_PPSSPP_MEMSTICK"?selected:"operator-home";}
#define getenv test_getenv
enum SystemProperty{SYSPROP_CAN_JIT,SYSPROP_IS_HEADLESS,OTHER};
''' + function('static void ConfigureMemstick(') + '\n' + function('bool System_GetPropertyBool(') + '\n' + function('static bool PPGeShouldLoadAtlas()', atlas) + r'''
int main(){
 for(const char* root:{"generation-a/memstick","generation-b/memstick"}){
  selected=root;writes.clear();ConfigureMemstick(Path("native"));
  assert(g_Config.memStickDirectory.value==root);
  assert((writes==std::vector<std::string>{root,std::string(root)+"/PSP/SYSTEM"}));
  assert(System_GetPropertyBool(SYSPROP_IS_HEADLESS));
  assert(PPGeShouldLoadAtlas());
  assert(System_GetPropertyBool(SYSPROP_CAN_JIT));
  assert(!System_GetPropertyBool(OTHER));
 }
 for(const char* empty:std::vector<const char*>{nullptr,""}){
  selected=empty;writes.clear();ConfigureMemstick(Path("native"));
  assert(System_GetPropertyBool(SYSPROP_IS_HEADLESS));
  assert(!PPGeShouldLoadAtlas());
#if PPSSPP_PLATFORM_WINDOWS
  assert(g_Config.memStickDirectory.value=="native/memstick");
#else
  assert(g_Config.memStickDirectory.value=="operator-home/.ppsspp");
#endif
 }
}
'''
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / 'probe.cpp'
    binary = Path(directory) / 'probe'
    source.write_text(code)
    for windows in (0, 1):
        subprocess.run(['clang++', '-std=c++17', '-fsanitize=address,undefined',
                        f'-DPPSSPP_PLATFORM_WINDOWS={windows}', str(source), '-o', str(binary)], check=True)
        subprocess.run([str(binary)], check=True)
print('Managed headless dialogs and generation memstick isolation passed')
