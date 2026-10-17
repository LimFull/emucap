#!/usr/bin/env python3
"""Exercise native queued-step receipt ownership against a patched PPSSPP source tree."""
import argparse
from pathlib import Path
import subprocess
import tempfile
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,required=True)
a=p.parse_args()
s=(a.source/'Core/Core.cpp').read_text()
def extract(marker):
 start=s.index(marker); brace=s.index('{',start); end=brace+1; depth=1
 while depth:
  depth+=(s[end]=='{')-(s[end]=='}');end+=1
 return s[start:end]
code=r'''
#include <cassert>
#include <atomic>
#include <chrono>
#include <cstdlib>
#include <sys/wait.h>
#include <unistd.h>
#include <thread>
#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <vector>
#include <utility>
using u32=uint32_t;
enum class CPUStepType{None,Into,Over,Out,Frame};
enum class BreakReason{None,DebugStep,DebugStepInto,DebugBreak};
enum {CORE_STEPPING_CPU,CORE_STEPPING_GE,CORE_RUNNING_GE,CORE_REENTER_DISPATCH,CORE_NEXTFRAME,CORE_RUNNING_CPU};
int coreState=CORE_STEPPING_CPU,steppingCounter=0;
BreakReason g_breakReason=BreakReason::None;
struct BreakpointHit {int kind=0;} g_breakHit;
std::recursive_mutex g_stepMutex;
std::atomic<uint64_t> g_emucapMemoryEpoch{1},g_emucapPublishedMemoryEpoch{0};
uint64_t g_emucapFenceEpoch=0,g_emucapIoFence=0;
bool writerReady=true,stepStartsWriter=false;
uint64_t __IoBeginMemoryFence(){return 1;}
bool __IoMemoryFenceComplete(uint64_t){return writerReady;}
#define ERROR_LOG(...) ((void)0)
#define _dbg_assert_(v) assert(v)
void Core_WakeIdleCPUThread(){}
void Core_StateProcessed(){}
void Core_ResetException(){}
namespace SaveState {void Process(){}}
namespace GPUStepping {void ProcessStepping(){}}
enum class SystemNotification{DISASSEMBLY_AFTERSTEP,MEM_VIEW};
void System_Notify(SystemNotification){}
struct MIPSDebugInterface{u32 pc=0x1000;};
void Core_PerformCPUStep(MIPSDebugInterface *cpu,CPUStepType type){assert(type==CPUStepType::Into);cpu->pc+=4;if(stepStartsWriter)writerReady=false;}
'''+extract('void Core_EmucapInvalidateMemoryPark()')+'\n'+extract('static bool Core_EmucapPollMemoryPark(')+'\n'+extract('struct CPUStepCommand')+r''';
CPUStepCommand g_cpuStepCommand{};
std::deque<CPUStepCommand> g_cpuStepQueue;
constexpr size_t MAX_PENDING_STEPS=8;
enum class EmucapFrameStepStatus{Inactive,Running};
struct {EmucapFrameStepStatus status=EmucapFrameStepStatus::Inactive;} g_emucapFrameStep;
std::mutex g_emucapFrameStepMutex;
std::atomic<bool> g_emucapFrameOwned{false};
int breaks=0;
void Core_Break(BreakReason reason,u32){++breaks;g_breakReason=reason;coreState=CORE_STEPPING_CPU;Core_EmucapInvalidateMemoryPark();}
bool Core_IsActive(){return coreState==CORE_RUNNING_CPU;}
'''+extract('struct DebuggerStopOwner')+r''';
DebuggerStopOwner g_debuggerStop;
'''+extract('bool Core_EmucapMemoryParked()')+'\n'+extract('bool Core_HasDebuggerWork()')+'\n'+extract('bool Core_ArmDebuggerStop(')+'\n'+extract('bool Core_RequestDebuggerPause(')+'\n'+extract('static void Core_PollDebuggerStop()')+'\n'+extract('static bool Core_PublishDebuggerStop(')+r'''
'''+extract('bool Core_RequestCPUStep(')+'\n'+extract('static bool Core_ProcessStepping(MIPSDebugInterface *cpu) {')+r'''
int main(){
 MIPSDebugInterface cpu;
 std::vector<std::pair<int,u32>> receipts;
 auto ownerA=std::make_shared<int>(1),ownerB=std::make_shared<int>(2);
 auto submit=[&](int ticket,const std::shared_ptr<int>&owner){
  std::weak_ptr<int> weak=owner;
  return Core_RequestCPUStep(CPUStepType::Into,[&,weak,ticket]{
   // A real receipt may read the halt reason under this mutex. Completion must release it.
   bool unlocked=false;
   std::thread observer([&]{unlocked=g_stepMutex.try_lock();if(unlocked)g_stepMutex.unlock();});
   observer.join();assert(unlocked);
   if(!weak.expired()) receipts.emplace_back(ticket,cpu.pc);
  },[weak]{return !weak.expired();});
 };
 g_breakHit.kind=1;
 assert(submit(10,ownerA)&&submit(20,ownerB));
 assert(receipts.empty());
 Core_ProcessStepping(&cpu);assert(receipts.size()==1&&receipts[0]==std::make_pair(10,0x1004u));
 assert(g_breakHit.kind==0);
 Core_ProcessStepping(&cpu);assert(receipts.size()==2&&receipts[1]==std::make_pair(20,0x1008u));
 Core_ProcessStepping(&cpu);assert(receipts.size()==2); // an unrelated poll cannot replay a receipt
 assert(submit(30,ownerA));ownerA.reset();
 auto replacement=std::make_shared<int>(1); // same user identity, distinct connection lifetime
 assert(submit(40,replacement));
 Core_ProcessStepping(&cpu);assert(cpu.pc==0x1008&&receipts.size()==2);
 Core_ProcessStepping(&cpu);assert(receipts.back()==std::make_pair(40,0x100cu));
 for(int i=0;i<8;i++)assert(submit(100+i,ownerB));
 assert(!submit(999,ownerB)); // rejected queue admission has no execution or receipt
 for(int i=0;i<8;i++)Core_ProcessStepping(&cpu);
 assert(receipts.size()==11&&receipts.back().first==107);
 assert(!Core_RequestCPUStep(CPUStepType::Over,[]{},{}));
 assert(g_cpuStepQueue.empty());
 assert(submit(500,ownerB));coreState=CORE_RUNNING_CPU;
 Core_ProcessStepping(&cpu);assert(receipts.size()==11); // running observation is not step completion
 g_cpuStepQueue.clear();coreState=CORE_STEPPING_CPU; // teardown cancels pending native work
 Core_ProcessStepping(&cpu);assert(receipts.size()==11);
 // CPU retirement alone cannot complete a step that started an asynchronous RAM writer.
 assert(submit(600,ownerB)&&submit(601,ownerB));stepStartsWriter=true;
 const auto pcBefore=cpu.pc;
 Core_ProcessStepping(&cpu);assert(cpu.pc==pcBefore+4&&receipts.size()==11);
 Core_ProcessStepping(&cpu);assert(cpu.pc==pcBefore+4&&receipts.size()==11);
 writerReady=true;stepStartsWriter=false;
 Core_ProcessStepping(&cpu);assert(cpu.pc==pcBefore+4&&receipts.back().first==600);
 Core_ProcessStepping(&cpu);assert(cpu.pc==pcBefore+8&&receipts.back().first==601);
 auto arm=[&](int ticket,const std::shared_ptr<int>&owner,bool running=false){
  std::weak_ptr<int> weak=owner;
  return Core_ArmDebuggerStop([&,ticket]{
   bool unlocked=false;
   std::thread observer([&]{unlocked=g_stepMutex.try_lock();if(unlocked)g_stepMutex.unlock();});
   observer.join();assert(unlocked);receipts.emplace_back(ticket,cpu.pc);
  },[weak]{return !weak.expired();},running,false);
 };
 // Existing instructions or frame ownership reject a running plan before mutation.
 assert(submit(700,ownerB));assert(!arm(701,ownerB));Core_ProcessStepping(&cpu);
 g_emucapFrameStep.status=EmucapFrameStepStatus::Running;assert(!arm(702,ownerB));
 g_emucapFrameStep.status=EmucapFrameStepStatus::Inactive;
 g_emucapFrameOwned=true;assert(!arm(702,ownerB)&&!submit(702,ownerB));g_emucapFrameOwned=false;
 assert(arm(703,ownerB));assert(!arm(704,replacement));assert(!submit(705,replacement));
 coreState=CORE_RUNNING_CPU;writerReady=false;
 Core_Break(BreakReason::DebugBreak,0);Core_ProcessStepping(&cpu);assert(receipts.back().first==700);
 writerReady=true;Core_ProcessStepping(&cpu);assert(receipts.back().first==703);
 const auto delivered=receipts.size();Core_ProcessStepping(&cpu);assert(receipts.size()==delivered);
 assert(arm(706,replacement));coreState=CORE_RUNNING_CPU;replacement.reset();
 Core_PollDebuggerStop();assert(coreState==CORE_STEPPING_CPU);
 Core_ProcessStepping(&cpu);assert(receipts.size()==delivered&&!Core_HasDebuggerWork());
 // A host-frame handback must finish before a requested CPU pause can execute.
 coreState=CORE_NEXTFRAME;
 assert(Core_RequestDebuggerPause([&]{receipts.emplace_back(708,cpu.pc);},[]{return true;},BreakReason::DebugStep));
 Core_PollDebuggerStop();assert(coreState==CORE_NEXTFRAME);
 coreState=CORE_RUNNING_CPU;Core_PollDebuggerStop();assert(coreState==CORE_STEPPING_CPU);
 Core_ProcessStepping(&cpu);assert(receipts.back().first==708);
 // Host timeout owns the break; a failed cleanup cannot leave this generation reusable.
 assert(arm(707,ownerB));coreState=CORE_RUNNING_CPU;
 g_debuggerStop.deadline=std::chrono::steady_clock::now();
 Core_PollDebuggerStop();assert(g_debuggerStop.stopping&&coreState==CORE_STEPPING_CPU);
 writerReady=false;g_debuggerStop.cleanupDeadline=std::chrono::steady_clock::now();
 pid_t child=fork();assert(child>=0);
 if(!child){Core_PollDebuggerStop();_exit(7);}
 int status=0;assert(waitpid(child,&status,0)==child);assert(WIFSIGNALED(status)&&WTERMSIG(status)==SIGABRT);
 writerReady=true;Core_ProcessStepping(&cpu);assert(receipts.back().first==707);

}
'''
with tempfile.TemporaryDirectory() as d:
 source=Path(d)/'probe.cpp';binary=Path(d)/'probe';source.write_text(code)
 subprocess.run(['clang++','-std=c++17','-fsanitize=address,undefined',str(source),'-o',str(binary)],check=True)
 subprocess.run([str(binary)],check=True,timeout=15)
print('Native step receipts: queue order, boundary PC, disconnect, replacement, rejection, writer completion and teardown pass')
