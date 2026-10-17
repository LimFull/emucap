#!/usr/bin/env python3
"""Exercise the patched DeSmuME listener and authentication on real loopback sockets."""
import argparse
import hashlib
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=Path, required=True, help="patched gdbstub.cpp")
parser.add_argument("--work-dir", type=Path, required=True, help="disposable output parent")
parser.add_argument("--windows-object", action="store_true", help="compile the actual listener with Winsock using Zig; do not execute")
args = parser.parse_args()
source = args.source.read_text()


def extract(marker):
    start = source.index(marker)
    brace = source.index("{", start)
    depth, end = 1, brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


code = r'''
#ifdef WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#define SOCKET_TYPE SOCKET
#define SOCKLEN_T int
#define CLOSESOCKET closesocket
#define RECV(A,B,C) recv(A,(char*)B,C,0)
#else
#include <arpa/inet.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <sys/resource.h>
#include <unistd.h>
#define SOCKET_TYPE int
#define SOCKLEN_T socklen_t
#define CLOSESOCKET close
#define RECV read
#define WINAPI
#endif
#include <algorithm>
#include <atomic>
#include <cassert>
#include <chrono>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <future>
#include <string>
#include <thread>
#include <vector>
#include <cerrno>
#include <fcntl.h>
#define DEBUG_LOG(...) ((void)0)
#define LOG_ERROR(...) ((void)0)
#define BUFMAX 0x8000
#define IDLE_READ_STATE 0
#define MID_PACKET_READ_STATE 1
#define FIRST_CHECKSUM_READ_STATE 2
#define SECOND_CHECKSUM_READ_STATE 3
#define CPU_STOPPED_STUB_MESSAGE 1
#define TARGET_SIGNAL_INT 2
#define TARGET_SIGNAL_TRAP 5
#define TARGET_SIGNAL_ABRT 6
const char hexchars[] = "0123456789abcdef";
enum read_res_gdb { READ_NOT_FINISHED, READ_SOCKET_ERROR, READ_COMPLETE, READ_BREAK };
enum stop_type { STOP_UNKNOWN, STOP_HOST_BREAK, STOP_STEP_BREAK, STOP_BREAKPOINT,
                 STOP_WATCHPOINT, STOP_RWATCHPOINT, STOP_AWATCHPOINT };
struct packet_reader_gdb {
    int state=0, pos_index=0; uint8_t checksum=0, read_checksum=0, buffer[BUFMAX]{};
};
struct CpuControl {
    void *data=nullptr;
    void (*install_post_ex_fn)(void *, void (*)(void *), void *) = nullptr;
};
struct gdb_stub_state {
    SOCKET_TYPE listen_fd=(SOCKET_TYPE)-1, sock_fd=(SOCKET_TYPE)-1, ctl_pipe[2]{};
    int active=1, main_stop_flag=0;
    enum { STOPPED_GDB_STATE, RUNNING_GDB_STATE } ctl_stub_state=STOPPED_GDB_STATE;
    enum stop_type stop_type=STOP_HOST_BREAK;
    uint32_t stop_address=0;
    packet_reader_gdb rx_packet;
    CpuControl *cpu_ctrl=nullptr;
};
std::atomic<bool> emucap_pace_wake{false};
std::atomic<int> dispatched{0};
void break_execution(void *) {}
'''
socket_source = source
if args.windows_object:
    source = source[source.index("#ifdef WIN32\n#define RECV"):]
else:
    source = source[source.index("#define RECV(A,B,C) read"):]
code += extract("static int set_socket_nodelay(")
source = socket_source
code += extract("static int\nhex (")
code += extract("static enum read_res_gdb\nreadPacket_gdb(")
code += source[source.index("struct hidden_debug_out_packet {"):
               source.index("#define CHECKSUM_PART_SIZE 3")]
code += extract("static int\nputpacket (")
code += extract("static uint32_t\nmake_stop_packet(")
code += extract("static bool emucap_running_safe_packet(")
code += r'''
// Only emulator command execution is substituted. Framing, auth, admission,
// socket I/O, ACKs, stop-packet formatting and the listener loop are production code.
int processPacket_gdb(SOCKET_TYPE fd, const uint8_t *packet, gdb_stub_state *state) {
    ++dispatched;
    if (strcmp((const char *)packet, "c") == 0)
        state->ctl_stub_state = gdb_stub_state::RUNNING_GDB_STATE;
    auto out = getOutPacket(); memcpy(out->start_ptr, "OK", 2);
    return putpacket(fd, out, 2) < 0 ? -1 : 0;
}
'''
code += extract("static void\nWINAPI listenerThread_gdb(")
code += r'''
#ifndef WIN32
using Clock = std::chrono::steady_clock;
using namespace std::chrono_literals;
std::string frame(const std::string &body) {
    uint8_t sum=0; for (unsigned char c: body) sum+=c;
    return "$" + body + "#" + hexchars[sum>>4] + hexchars[sum&15];
}
void send_bytes(int fd, const std::string &s) {
    assert(emucap_send_all_until(fd, s.data(), s.size(), Clock::now()+1s)==0);
}
char byte(int fd) { char c; assert(recv(fd, &c, 1, 0)==1); return c; }
std::string response(int fd, bool ack=true) {
    assert(byte(fd)=='$'); std::string s; char c;
    while ((c=byte(fd))!='#') s+=c;
    byte(fd); byte(fd); if (ack) send_bytes(fd, "+"); return s;
}
void authenticate(int fd) {
    send_bytes(fd, frame("qEmucap,auth:746573742d736563726574"));
    assert(byte(fd)=='+'); assert(response(fd)=="OK");
}
void command(int fd, const char *s) {
    send_bytes(fd, frame(s)); assert(byte(fd)=='+'); assert(response(fd)=="OK");
}
void closed(int fd) {
    char c; int n=recv(fd,&c,1,0);
    assert(n==0 || (n<0 && errno==ECONNRESET));
}
struct Listener {
    gdb_stub_state state; sockaddr_in addr{}; std::future<void> worker;
    explicit Listener(bool running=false) {
        state.listen_fd=socket(AF_INET,SOCK_STREAM,0); assert(state.listen_fd>=0);
        addr.sin_family=AF_INET; addr.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
        assert(bind(state.listen_fd,(sockaddr*)&addr,sizeof(addr))==0);
        socklen_t n=sizeof(addr); assert(getsockname(state.listen_fd,(sockaddr*)&addr,&n)==0);
        assert(listen(state.listen_fd,5)==0); assert(pipe(state.ctl_pipe)==0);
        if (running) state.ctl_stub_state=gdb_stub_state::RUNNING_GDB_STATE;
        worker=std::async(std::launch::async,[this]{listenerThread_gdb(&state);});
    }
    int connect_peer() {
        int fd=socket(AF_INET,SOCK_STREAM,0); assert(fd>=0);
        assert(connect(fd,(sockaddr*)&addr,sizeof(addr))==0);
        assert(set_socket_nodelay(fd)==0);
        timeval timeout{4,0};
        assert(setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof(timeout))==0);
        return fd;
    }
    void control(uint8_t value) { assert(write(state.ctl_pipe[1],&value,1)==1); }
    void quit() {
        control(0); assert(worker.wait_for(500ms)==std::future_status::ready);
        worker.get(); close(state.ctl_pipe[0]); close(state.ctl_pipe[1]);
    }
};
int main() {
    signal(SIGPIPE,SIG_IGN); setenv("EMUCAP_NATIVE_DEBUG_TOKEN","test-secret",1);
    {
        Listener server;
        int wrong=server.connect_peer();
        send_bytes(wrong,frame("qEmucap,auth:626164"));
        assert(byte(wrong)=='+'); closed(wrong); close(wrong);
        assert(dispatched==0);
        int unauth=server.connect_peer();
        send_bytes(unauth,frame("g"));
        assert(byte(unauth)=='+'); closed(unauth); close(unauth);
        assert(dispatched==0);
        int active=server.connect_peer(); authenticate(active); command(active,"g");
        for (int i=0;i<16;i++) {
            int candidate=server.connect_peer();
            auto start=Clock::now();
            command(active,"qEmucap,rateInfo");
            closed(candidate); close(candidate);
            assert(Clock::now()-start<500ms);
        }
        command(active,"c");
        int candidate=server.connect_peer(); auto start=Clock::now();
        command(active,"qEmucap,rateInfo");
        server.control(CPU_STOPPED_STUB_MESSAGE);
        assert(response(active)=="S02"); closed(candidate); close(candidate);
        assert(Clock::now()-start<500ms);
        server.quit(); closed(active); close(active);
        assert(dispatched==19);
    }
    {
        Listener server;
        int candidate=server.connect_peer();
        send_bytes(candidate,frame("qEmucap,auth:746573742d736563726574"));
        assert(byte(candidate)=='+');
        // Receiving OK without ACK proves entry into the actual authentication
        // exchange. Quit must preempt its ACK wait, not just an unaccepted socket.
        assert(response(candidate,false)=="OK");
        server.quit(); closed(candidate); close(candidate);
    }
    {
        // A silent peer must not delay a valid first attachment until its own timeout.
        Listener server;
        auto start=Clock::now(); int silent=server.connect_peer();
        std::this_thread::sleep_for(50ms);
        int valid=server.connect_peer(); authenticate(valid); command(valid,"g");
        auto elapsed=std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now()-start);
        printf("first attachment behind silent peer: %lld ms\n", (long long)elapsed.count()); fflush(stdout);
        assert(elapsed<500ms);
        closed(silent); close(silent); server.quit(); close(valid);
        printf("first attachment after silent candidate: %lld ms\n",(long long)elapsed.count());
    }
    {
        // Multiple silent candidates must permit a new valid peer before their deadlines.
        Listener server; int silent[3];
        for (int &fd: silent) fd=server.connect_peer();
        int valid=server.connect_peer();
        auto start=Clock::now(); authenticate(valid); command(valid,"g");
        auto elapsed=std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now()-start);
        assert(elapsed<500ms);
        server.quit(); close(valid);
        for (int fd: silent) close(fd);
        printf("first attachment behind three candidates: %lld ms\n",(long long)elapsed.count());
    }
    {
        Listener server; std::vector<int> silent;
        for (int i=0;i<16;++i) silent.push_back(server.connect_peer());
        auto start=Clock::now();
        int valid=server.connect_peer(); authenticate(valid); command(valid,"g");
        assert(Clock::now()-start<500ms);
        // Promotion retires every other pending candidate, including a full queue.
        for (int fd:silent) { closed(fd); close(fd); }
        close(valid);
        int replacement=server.connect_peer(); authenticate(replacement); command(replacement,"g");
        server.quit(); closed(replacement); close(replacement);
        puts("full queue, promotion cleanup and reconnect: passed");
    }
    {
        // A peer withholding the auth-OK ACK cannot reserve the debugger slot.
        Listener server; int waiting=server.connect_peer();
        send_bytes(waiting,frame("qEmucap,auth:746573742d736563726574"));
        assert(byte(waiting)=='+'); assert(response(waiting,false)=="OK");
        auto start=Clock::now();
        int valid=server.connect_peer(); authenticate(valid); command(valid,"g");
        assert(Clock::now()-start<500ms);
        closed(waiting); close(waiting); server.quit(); close(valid);
    }
    {
        // A token-verified peer keeps its bounded ACK flight when unauthenticated
        // arrivals fill the remaining candidate slots.
        Listener server; int verified=server.connect_peer();
        send_bytes(verified,frame("qEmucap,auth:746573742d736563726574"));
        assert(byte(verified)=='+'); assert(response(verified,false)=="OK");
        std::vector<int> silent;
        for (int i=0;i<8;++i) silent.push_back(server.connect_peer());
        std::this_thread::sleep_for(50ms);
        send_bytes(verified,"+"); command(verified,"g");
        server.quit(); close(verified); for (int fd:silent) close(fd);
    }
    {
        // A full verified queue rejects a new unauthenticated peer. Each verified
        // reservation still has the original absolute deadline.
        Listener server; std::vector<int> verified;
        const auto auth=frame("qEmucap,auth:746573742d736563726574");
        for (int i=0;i<8;++i) {
            int fd=server.connect_peer(); send_bytes(fd,auth);
            assert(byte(fd)=='+'); assert(response(fd,false)=="OK");
            verified.push_back(fd);
        }
        int rejected=server.connect_peer(); closed(rejected); close(rejected);
        send_bytes(verified[0],"+"); command(verified[0],"g");
        for (size_t i=1;i<verified.size();++i) { closed(verified[i]); close(verified[i]); }
        server.quit(); close(verified[0]);
    }
    {
        // The final ACK and first ordinary packet may share one TCP write.
        Listener server; int valid=server.connect_peer();
        send_bytes(valid,frame("qEmucap,auth:746573742d736563726574"));
        assert(byte(valid)=='+'); assert(response(valid,false)=="OK");
        send_bytes(valid,"+"+frame("g"));
        assert(byte(valid)=='+'); assert(response(valid)=="OK");
        server.quit(); close(valid);
    }
    {
        Listener server; int waiting=server.connect_peer();
        auto start=Clock::now();
        send_bytes(waiting,frame("qEmucap,auth:746573742d736563726574"));
        assert(byte(waiting)=='+'); assert(response(waiting,false)=="OK");
        closed(waiting);
        assert(Clock::now()-start>=1800ms && Clock::now()-start<3s);
        close(waiting);
        int replacement=server.connect_peer(); authenticate(replacement); command(replacement,"g");
        server.quit(); close(replacement);
    }
    {
        Listener server; int valid=server.connect_peer();
        send_bytes(valid,frame("qEmucap,auth:746573742d736563726574"));
        assert(byte(valid)=='+'); assert(response(valid,false)=="OK");
        send_bytes(valid,"-"); assert(response(valid,false)=="OK");
        send_bytes(valid,"?"); assert(response(valid,false)=="OK");
        send_bytes(valid,"-"); closed(valid); close(valid); server.quit();
    }
    {
        // Token comparison must not accept a valid prefix followed by an embedded NUL.
        const std::string auth="qEmucap,auth:746573742d736563726574";
        Listener server;
        int nul=server.connect_peer(); send_bytes(nul,frame(auth+std::string("\0trailing",9)));
        closed(nul); close(nul);
        int huge=server.connect_peer(); send_bytes(huge,frame("qEmucap,auth:"+std::string(1025,'7')));
        closed(huge); close(huge);
        int malformed=server.connect_peer(); send_bytes(malformed,frame("qEmucap,auth:746573742d73656372657z"));
        assert(byte(malformed)=='+'); closed(malformed); close(malformed);
        int checksum=server.connect_peer(); std::string corrupt=frame(auth);
        corrupt.back()=corrupt.back()=='0' ? '1' : '0';
        send_bytes(checksum,corrupt); assert(byte(checksum)=='-'); closed(checksum); close(checksum);
        int valid=server.connect_peer(); authenticate(valid); command(valid,"g");
        server.quit(); close(valid);
    }
    {
        // A complete maximum auth packet fits the candidate's per-turn receive budget.
        const std::string token(EMUCAP_GDB_AUTH_MAX_TOKEN_BYTES,'x');
        setenv("EMUCAP_NATIVE_DEBUG_TOKEN",token.c_str(),1);
        Listener server; std::vector<int> silent;
        for (int i=0;i<8;++i) silent.push_back(server.connect_peer());
        int valid=server.connect_peer();
        std::string encoded; for (char ch:token) { encoded+=hexchars[(unsigned char)ch>>4]; encoded+=hexchars[ch&15]; }
        const std::string packet=frame("qEmucap,auth:"+encoded);
        for (size_t offset=0;offset<packet.size();offset+=7) {
            send_bytes(valid,packet.substr(offset,7)); std::this_thread::sleep_for(1ms);
        }
        assert(byte(valid)=='+'); assert(response(valid)=="OK"); command(valid,"g");
        server.quit(); close(valid); for (int fd:silent) close(fd);
        setenv("EMUCAP_NATIVE_DEBUG_TOKEN","test-secret",1);
    }
    {
        Listener server; std::atomic<bool> started{false};
        auto churn=std::async(std::launch::async,[&]{
            std::vector<int> held;
            for (int i=0;i<96;++i) {
                int fd=server.connect_peer(); held.push_back(fd);
                if (i%2) send_bytes(fd,"x");
                if (i==8) started=true;
                std::this_thread::sleep_for(2ms);
            }
            for (int fd:held) close(fd);
        });
        while (!started) std::this_thread::yield();
        int valid=server.connect_peer(); auto start=Clock::now();
        authenticate(valid); command(valid,"g"); assert(Clock::now()-start<500ms);
        churn.get(); command(valid,"g"); server.quit(); close(valid);
        puts("finite invalid churn and established traffic: passed");
    }
    {
        // Stop is serviced during partial authentication, without disclosing a stop packet.
        Listener server(true); int valid=server.connect_peer();
        const std::string auth=frame("qEmucap,auth:746573742d736563726574");
        send_bytes(valid,auth.substr(0,9)); server.control(CPU_STOPPED_STUB_MESSAGE);
        send_bytes(valid,auth.substr(9)); assert(byte(valid)=='+'); assert(response(valid)=="OK");
        command(valid,"g"); server.quit();
        assert(server.state.ctl_stub_state==gdb_stub_state::STOPPED_GDB_STATE);
        assert(server.state.main_stop_flag==1); close(valid);
    }
    {
        // Two valid candidates may finish the token flight, but only one owns the debugger.
        Listener server; int first=server.connect_peer(), second=server.connect_peer();
        const auto auth=frame("qEmucap,auth:746573742d736563726574");
        send_bytes(first,auth); send_bytes(second,auth);
        assert(byte(first)=='+'); assert(response(first,false)=="OK");
        assert(byte(second)=='+'); assert(response(second,false)=="OK");
        send_bytes(first,"+"); command(first,"g"); closed(second);
        close(second); server.quit(); close(first);
    }
    {
        // Check the POSIX numeric select boundary before FD_SET, preserving the original FD.
        rlimit limits; assert(getrlimit(RLIMIT_NOFILE,&limits)==0);
        assert(limits.rlim_max>(rlim_t)FD_SETSIZE);
        rlimit raised=limits; raised.rlim_cur=std::max(raised.rlim_cur,(rlim_t)FD_SETSIZE+1);
        assert(setrlimit(RLIMIT_NOFILE,&raised)==0);
        int sockets[2]; assert(socketpair(AF_UNIX,SOCK_STREAM,0,sockets)==0);
        int high=fcntl(sockets[0],F_DUPFD,FD_SETSIZE); assert(high>=FD_SETSIZE);
        EmucapGdbAdmission candidates; candidates.add(high);
        assert(fcntl(high,F_GETFD)==-1 && errno==EBADF);
        assert(fcntl(sockets[0],F_GETFD)>=0);
        close(sockets[0]); close(sockets[1]); assert(setrlimit(RLIMIT_NOFILE,&limits)==0);
    }
    puts("NDS listener: established peer, candidate rejection, stop and quit isolation passed");
}
#endif
'''
args.work_dir.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="nds-gdb-admission-", dir=args.work_dir) as tmp:
    root = Path(tmp)
    (root / "probe.cpp").write_text(code)
    if args.windows_object:
        subprocess.run(["zig", "c++", "-target", "x86_64-windows-gnu", "-DWIN32",
                        "-std=c++14", "-I", str(args.source.parent), "-c",
                        str(root / "probe.cpp"), "-o", str(root / "probe.o")], check=True)
        print("Windows x64 actual listener/candidate object compilation: passed")
    else:
        subprocess.run(["c++", "-std=c++14", "-pthread", "-fsanitize=address,undefined",
                        "-I", str(args.source.parent), str(root / "probe.cpp"), "-o", str(root / "probe")], check=True)
        subprocess.run([str(root / "probe")], check=True, timeout=20)
print("patched source sha256:", hashlib.sha256(args.source.read_bytes()).hexdigest())
