#include "emucap_control_io.h"
#include <cassert>
#include <atomic>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <thread>
using emucap::control_socket;
namespace fs = std::filesystem;

template<class F> void rejects(F action)
{
	bool rejected = false;
	try { action(); } catch (std::exception const &) { rejected = true; }
	assert(rejected);
}

static void append_field(util::sha1_creator &hash, std::string_view field)
{
	std::array<std::uint8_t, 8> length;
	auto value = std::uint64_t(field.size());
	for (int index = 7; index >= 0; --index)
	{
		length[index] = std::uint8_t(value);
		value >>= 8;
	}
	hash.append(length.data(), length.size());
	hash.append(field.data(), field.size());
}

static std::string proof(std::string_view token, std::string_view direction,
		std::string_view client_nonce, std::string_view server_nonce)
{
	std::array<std::uint8_t, 64> key{};
	if (token.size() > key.size())
	{
		auto const digest = util::sha1_creator::simple(token.data(), token.size());
		std::copy(std::begin(digest.m_raw), std::end(digest.m_raw), key.begin());
	}
	else
	{
		std::copy(token.begin(), token.end(), key.begin());
	}
	std::array<std::uint8_t, 64> inner_pad, outer_pad;
	for (std::size_t index = 0; index < key.size(); ++index)
	{
		inner_pad[index] = key[index] ^ 0x36;
		outer_pad[index] = key[index] ^ 0x5c;
	}
	util::sha1_creator inner;
	inner.append(inner_pad.data(), inner_pad.size());
	append_field(inner, "emucap-mame-native-auth-v1");
	append_field(inner, direction);
	append_field(inner, client_nonce);
	append_field(inner, server_nonce);
	auto const inner_digest = inner.finish();
	util::sha1_creator outer;
	outer.append(outer_pad.data(), outer_pad.size());
	outer.append(inner_digest.m_raw, sizeof(inner_digest.m_raw));
	return outer.finish().as_string();
}

static std::string frame(std::string_view payload)
{
	std::uint8_t sum = 0;
	for (auto const ch : payload) sum += std::uint8_t(ch);
	char checksum[3];
	std::snprintf(checksum, sizeof(checksum), "%02x", sum);
	return "$" + std::string(payload) + "#" + checksum;
}

static std::string reply(asio::ip::tcp::socket &client, bool acknowledge = true)
{
	char byte;
	asio::read(client, asio::buffer(&byte, 1));
	assert(byte == '+');
	asio::read(client, asio::buffer(&byte, 1));
	assert(byte == '$');
	std::string payload;
	for (;;)
	{
		asio::read(client, asio::buffer(&byte, 1));
		if (byte == '#') break;
		payload.push_back(byte);
	}
	std::array<char, 2> checksum;
	asio::read(client, asio::buffer(checksum));
	assert(frame(payload).substr(payload.size() + 2) == std::string(checksum.data(), checksum.size()));
	if (acknowledge) asio::write(client, asio::buffer("+", 1));
	return payload;
}

static void authenticate(asio::ip::tcp::socket &client, std::string_view token)
{
	std::string const client_nonce = "102132435465768798a9bacbdcedfe0f";
	auto const first = frame("qEmucap,auth1:" + client_nonce);
	asio::write(client, asio::buffer(first));
	auto const challenge = reply(client);
	std::string const prefix = "qEmucap,auth2:";
	assert(challenge.starts_with(prefix));
	auto const separator = challenge.find(':', prefix.size());
	assert(separator != std::string::npos);
	auto const server_nonce = challenge.substr(prefix.size(), separator - prefix.size());
	auto const server_proof = challenge.substr(separator + 1);
	assert(server_nonce.size() == 32);
	assert(server_proof == proof(token, "server", client_nonce, server_nonce));
	auto const third = frame("qEmucap,auth3:" + proof(token, "client", client_nonce, server_nonce));
	asio::write(client, asio::buffer(third));
	assert(reply(client) == "OK");
}

int main(int argc, char **argv)
{
	assert(argc == 2);
	auto const root = fs::path(argv[1]);
	auto const file = root / "frame.bin";
	std::string const bytes("a\0b\1c\2d\3", 8);
	{ std::ofstream output(file, std::ios::binary); output.write(bytes.data(), bytes.size()); }
	assert(emucap::read_framebuffer(file.string(), 2, 1) == bytes);
	rejects([&] { emucap::read_framebuffer(file.string(), 0, 1); });
	rejects([&] { emucap::read_framebuffer(file.string(), 0xffffffff, 0xffffffff); });
	rejects([&] { emucap::read_framebuffer(file.string(), 1, 1); });
	rejects([&] { emucap::read_framebuffer(file.string(), 3, 1); });
	rejects([&] { emucap::read_framebuffer(root.string(), 2, 1); });
	rejects([&] { emucap::read_framebuffer(file.string() + std::string("\0suffix", 7), 2, 1); });
	auto const sparse = root / "large.bin";
	{ std::ofstream output(sparse, std::ios::binary); output.seekp(1ULL << 32); output.put('x'); }
	rejects([&] { emucap::read_framebuffer(sparse.string(), 2, 1); });
	fs::resize_file(sparse, 16 * 1024 * 1024);
	auto const maximum = emucap::read_framebuffer(sparse.string(), 4194304, 1);
	assert(maximum.size() == 16 * 1024 * 1024 && maximum.front() == 0 && maximum.back() == 0);
#if !defined(_WIN32)
	auto const fifo = root / "fifo";
	assert(!mkfifo(fifo.c_str(), 0600));
	auto const file_start = control_socket::now_ms();
	rejects([&] { emucap::read_framebuffer(fifo.string(), 2, 1); });
	assert(control_socket::now_ms() - file_start < 1000);
	auto const link = root / "link";
	fs::create_symlink(file, link);
	rejects([&] { emucap::read_framebuffer(link.string(), 2, 1); });
#endif
	rejects([] { control_socket invalid(65536, "token"); });
	rejects([] { control_socket invalid(0, ""); });
	rejects([] { control_socket invalid(0, std::string(257, 'x')); });
	asio::io_context context;
	asio::ip::tcp::socket client(context);
	std::string const short_token = "native-control-test-token";
	std::string const token = "native-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
	assert(token.size() == 71);
	assert(util::sha1_creator::simple("abc", 3).as_string()
		== "a9993e364706816aba3e25717850c26c9cd0d89d");
	auto const vector_proof = proof(short_token, "server", "102132435465768798a9bacbdcedfe0f",
		"00112233445566778899aabbccddeeff");
	if (vector_proof != "cd4b1bb6f36d7577ff4df790e3b62b98973be1c5")
		std::cerr << "native HMAC vector mismatch: " << vector_proof << '\n';
	assert(vector_proof == "cd4b1bb6f36d7577ff4df790e3b62b98973be1c5");
	assert(proof(token, "server", "102132435465768798a9bacbdcedfe0f",
		"00112233445566778899aabbccddeeff")
		== "bfd0896d0923c875b3677c82eaacd43d285982e6");
	control_socket server(0, token);
	rejects([&] { control_socket duplicate(server.port(), token); });
	assert(std::get<0>(server.read()) == "idle");
	assert(std::get<0>(server.write("x")) == "idle");
	asio::ip::tcp::socket attacker(context);
	attacker.connect({asio::ip::address_v4::loopback(), server.port()});
	auto const unauthorized = frame("?");
	asio::write(attacker, asio::buffer(unauthorized));
	for (int i = 0; i < 20; ++i)
	{
		assert(std::get<0>(server.read()) == "idle");
		control_socket::wait();
	}
	attacker.non_blocking(true);
	std::array<char, 1> rejected_byte;
	asio::error_code rejected_error;
	assert(attacker.read_some(asio::buffer(rejected_byte), rejected_error) == 0);
	assert(rejected_error == asio::error::eof || rejected_error == asio::error::connection_reset);
	client.connect({asio::ip::address_v4::loopback(), server.port()});
	std::string accepted, detail;
	std::thread service([&] {
		for (int i = 0; i < 1000; ++i)
		{
			std::tie(accepted, detail) = server.read();
			if (accepted != "idle") return;
			control_socket::wait();
		}
	});
	authenticate(client, token);
	service.join();
	if (accepted != "accepted") std::cerr << accepted << ": " << detail << '\n';
	assert(accepted == "accepted");
	assert(std::get<0>(server.read()) == "idle");
	asio::ip::tcp::socket second(context);
	asio::error_code error;
	second.connect({asio::ip::address_v4::loopback(), server.port()}, error);
	assert(error);
	std::string const input(9000, 'x');
	asio::write(client, asio::buffer(input));
	std::string received;
	for (int i = 0; received.size() < input.size() && i < 100; ++i)
	{
		auto const [state, data] = server.read();
		assert(state == "data" || state == "idle");
		if (state == "idle") control_socket::wait();
		assert(data.size() <= control_socket::chunk_limit);
		received += data;
	}
	assert(received == input);
	rejects([&] { server.write(std::string(4097, 'x')); });
	assert(std::get<0>(server.write("ok")) == "data");
	std::array<char, 2> wire_reply;
	asio::read(client, asio::buffer(wire_reply));
	assert(std::string(wire_reply.data(), wire_reply.size()) == "ok");
	// A peer that stops reading must turn write into idle rather than blocking the host.
	bool saturated = false;
	auto const start = control_socket::now_ms();
	for (int i = 0; i < 16384; ++i)
	{
		auto const [state, count] = server.write(std::string(4096, 'x'));
		assert(count <= 4096);
		if (state == "idle") { saturated = true; break; }
		assert(state == "data");
	}
	assert(saturated && control_socket::now_ms() - start < 5000);
	for (int i = 0; i < 8; ++i) server.write("x");
	client.close();
	std::string terminal;
	for (int i = 0; i < 200; ++i)
	{
		terminal = std::get<0>(server.read());
		if (terminal != "idle") break;
		control_socket::wait();
	}
	assert(terminal == "closed" || terminal == "error");
	assert(std::get<0>(server.read()) == "closed");
	assert(std::get<0>(server.write("x")) == "closed");
	server.close();
#if !defined(_WIN32)
	// Managed launches reuse a stable port. A server-first close must not leave
	// TIME_WAIT preventing the replacement generation from binding that port.
	control_socket retiring(0, token);
	auto const restart_port = retiring.port();
	asio::ip::tcp::socket retiring_client(context);
	retiring_client.connect({asio::ip::address_v4::loopback(), restart_port});
	std::thread retiring_service([&] {
	for (int i = 0; i < 200; ++i)
	{
		std::tie(accepted, detail) = retiring.read();
		if (accepted != "idle") return;
		control_socket::wait();
	}
	});
	authenticate(retiring_client, token);
	retiring_service.join();
	assert(accepted == "accepted");
	retiring.close();
	std::array<char, 1> eof;
	assert(retiring_client.read_some(asio::buffer(eof), error) == 0);
	assert(error == asio::error::eof);
	retiring_client.close();
	control_socket restarted(restart_port, token);
	assert(restarted.port() == restart_port);
	rejects([&] { control_socket duplicate(restart_port, token); });
#endif
	// A valid peer can enter through a full silent pool. Once its client proof
	// is verified, later unauthenticated arrivals cannot evict its final ACK
	// flight, and bytes pipelined after that ACK are preserved for Lua.
	control_socket guarded(0, token);
	std::vector<std::unique_ptr<asio::ip::tcp::socket>> silent;
	for (std::size_t index = 0; index < control_socket::candidate_limit; ++index)
	{
		auto candidate = std::make_unique<asio::ip::tcp::socket>(context);
		candidate->connect({asio::ip::address_v4::loopback(), guarded.port()});
		silent.emplace_back(std::move(candidate));
		guarded.read();
	}
	asio::ip::tcp::socket verified(context);
	verified.connect({asio::ip::address_v4::loopback(), guarded.port()});
	std::string guarded_state, guarded_detail;
	std::thread guarded_service([&] {
		for (int i = 0; i < 2000; ++i)
		{
			std::tie(guarded_state, guarded_detail) = guarded.read();
			if (guarded_state != "idle") return;
			control_socket::wait();
		}
	});
	std::string const client_nonce = "102132435465768798a9bacbdcedfe0f";
	auto const first = frame("qEmucap,auth1:" + client_nonce);
	asio::write(verified, asio::buffer(first));
	auto const challenge = reply(verified);
	std::string const prefix = "qEmucap,auth2:";
	auto const separator = challenge.find(':', prefix.size());
	assert(challenge.starts_with(prefix) && separator != std::string::npos);
	auto const server_nonce = challenge.substr(prefix.size(), separator - prefix.size());
	assert(challenge.substr(separator + 1) == proof(token, "server", client_nonce, server_nonce));
	auto const third = frame("qEmucap,auth3:" + proof(token, "client", client_nonce, server_nonce));
	asio::write(verified, asio::buffer(third));
	assert(reply(verified, false) == "OK");
	for (std::size_t index = 0; index < control_socket::candidate_limit; ++index)
	{
		auto candidate = std::make_unique<asio::ip::tcp::socket>(context);
		candidate->connect({asio::ip::address_v4::loopback(), guarded.port()});
		silent.emplace_back(std::move(candidate));
		control_socket::wait();
	}
	auto const pipelined = frame("?");
	asio::write(verified, asio::buffer("+" + pipelined));
	guarded_service.join();
	assert(guarded_state == "accepted");
	auto const [pipelined_state, pipelined_bytes] = guarded.read();
	assert(pipelined_state == "data" && pipelined_bytes == pipelined);
	guarded.close();
	for (int i = 0; i < 512; ++i) { control_socket temporary(0, token); }
	std::cout << "native control: mutual admission, loopback lifecycle, EOF, same-port restart, chunk limits, backpressure and bounded regular files passed\n";
}
