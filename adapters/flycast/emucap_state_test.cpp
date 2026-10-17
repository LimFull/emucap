#include "emucap_state.h"
#include <cassert>
#include <cstdio>
#include <filesystem>
#include <limits>
int main(int argc, char** argv) {
    assert(argc == 2);
    assert(!emucap_state_payload_size_valid(0));
    assert(!emucap_state_payload_size_valid(7));
    assert(emucap_state_payload_size_valid(8));
    assert(emucap_state_payload_size_valid(EMUCAP_FLYCAST_STATE_MAX - 24));
    assert(!emucap_state_payload_size_valid(EMUCAP_FLYCAST_STATE_MAX - 23));
    assert(!emucap_state_payload_size_valid(std::numeric_limits<std::size_t>::max()));
    for (unsigned unit = 0; unit <= 5; ++unit) for (unsigned memory = 0; memory <= 4; ++memory) {
        std::vector<std::uint8_t> bytes(EMUCAP_FLYCAST_STATE_HEADER + 32, 0x79);
        emucap_state_header(bytes, unit, memory);
        std::uint32_t u = 99, m = 99;
        assert(emucap_state_parse(bytes, u, m) && u == unit && m == memory);
        for (unsigned i = EMUCAP_FLYCAST_STATE_HEADER; i < bytes.size(); ++i) assert(bytes[i] == 0x79);
        for (unsigned length = 0; length < bytes.size(); ++length) {
            auto short_bytes = bytes; short_bytes.resize(length); u = m = 99;
            assert(!emucap_state_parse(short_bytes, u, m) && u == 99 && m == 99);
        }
        for (unsigned offset : {0u, 8u, 12u, 16u}) {
            auto bad = bytes; bad[offset] = 0xff; u = m = 99;
            assert(!emucap_state_parse(bad, u, m) && u == 99 && m == 99);
        }
        bytes.push_back(0); u = m = 99;
        assert(!emucap_state_parse(bytes, u, m) && u == 99 && m == 99);
    }
    const auto path = (std::filesystem::path(argv[1]) / "flycast-state.mcs").string();
    auto write = [&](const std::vector<std::uint8_t>& bytes) {
        FILE* f = fopen(path.c_str(), "wb"); assert(f);
        assert(fwrite(bytes.data(), 1, bytes.size(), f) == bytes.size());
        assert(fclose(f) == 0);
    };
    auto rejects = [&](const char* name) {
        try { emucap_state_read(name); } catch (const std::exception&) { return; }
        assert(false && "invalid file accepted");
    };
    std::vector<std::uint8_t> bytes(56, 0x79);
    emucap_state_header(bytes, 5, 4);
    write(bytes);
    assert(emucap_state_read(path.c_str()) == bytes);
    rejects(argv[1]); // Directory, not a regular artifact.
    bytes[0] = 0; write(bytes); rejects(path.c_str());
    bytes.resize(25); emucap_state_header(bytes, 0, 0);
    write(bytes); rejects(path.c_str()); // Native constructor requires eight bytes.
    bytes.resize(56); emucap_state_header(bytes, 0, 0);
    bytes.pop_back(); write(bytes); rejects(path.c_str());
    bytes.push_back(0); bytes.push_back(0); write(bytes); rejects(path.c_str());
#ifndef _WIN32
    FILE* sparse = fopen(path.c_str(), "wb"); assert(sparse);
    assert(ftruncate(fileno(sparse), 4LL * 1024 * 1024 * 1024) == 0);
    fclose(sparse); rejects(path.c_str());
    // Even a within-budget sparse file must pass header admission before allocation.
    sparse = fopen(path.c_str(), "wb"); assert(sparse);
    assert(ftruncate(fileno(sparse), EMUCAP_FLYCAST_STATE_MAX) == 0);
    fclose(sparse); rejects(path.c_str());
    std::filesystem::remove(path);
    assert(mkfifo(path.c_str(), 0600) == 0);
    rejects(path.c_str()); // Must not wait for a FIFO writer.
#endif
    std::filesystem::remove(path);
    std::puts("FLYCAST STATE ENVELOPE PASSED");
}
