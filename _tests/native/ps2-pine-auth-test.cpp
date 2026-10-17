#include "EmuCapPineAuth.h"

#include <algorithm>
#include <array>
#include <cassert>
#include <cstdint>
#include <string>
#include <vector>

using namespace EmuCap::PineAuth;

static std::vector<std::uint8_t> Packet(const std::vector<std::uint8_t>& payload)
{
	std::vector<std::uint8_t> packet;
	AppendU32(packet, static_cast<std::uint32_t>(4 + payload.size()));
	packet.insert(packet.end(), payload.begin(), payload.end());
	return packet;
}

static std::vector<std::uint8_t> Drain(ServerSession& session, std::size_t fragment)
{
	std::vector<std::uint8_t> bytes;
	while (session.PendingSize() != 0)
	{
		const std::size_t count = std::min(fragment, session.PendingSize());
		bytes.insert(bytes.end(), session.PendingData(), session.PendingData() + count);
		assert(session.Wrote(count));
	}
	return bytes;
}

int main()
{
	const std::string token = "native-" + std::string(4, '\0');
	const std::string production_token =
		"native-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
	assert(production_token.size() == 71);
	Nonce vector_client{};
	Nonce vector_server{};
	for (std::size_t index = 0; index < NonceSize; ++index)
	{
		vector_client[index] = static_cast<std::uint8_t>(index);
		vector_server[index] = static_cast<std::uint8_t>(index + NonceSize);
	}
	const Proof vector = MakeProof(production_token, "server", vector_client, vector_server);
	const std::array<std::uint8_t, ProofSize> expected = {
		0xee, 0x70, 0x12, 0xc0, 0x07, 0xb5, 0xc3, 0x58,
		0x7e, 0xc1, 0x26, 0xe6, 0xd0, 0xd7, 0x8c, 0x25,
		0x30, 0x29, 0xc8, 0x83, 0x88, 0x3c, 0x85, 0x09,
		0xb2, 0xf3, 0xfc, 0xe6, 0x33, 0x17, 0xce, 0x82,
	};
	assert(ConstantTimeEqual(vector.data(), expected.data(), expected.size()));

	Nonce client_nonce{};
	for (std::size_t index = 0; index < client_nonce.size(); ++index)
		client_nonce[index] = static_cast<std::uint8_t>(0xa0 + index);
	std::vector<std::uint8_t> challenge{ChallengeCommand};
	challenge.insert(challenge.end(), client_nonce.begin(), client_nonce.end());
	const auto challenge_packet = Packet(challenge);
	ServerSession session(production_token);
	std::uint64_t counter = 0;
	for (std::uint8_t byte : challenge_packet)
	{
		assert(session.Feed(&byte, 1));
		assert(session.Advance(counter));
	}
	const auto challenge_reply = Drain(session, 3);
	assert(challenge_reply.size() == 5 + NonceSize + ProofSize);
	assert(ReadU32(challenge_reply.data()) == challenge_reply.size());
	assert(challenge_reply[4] == 0);
	Nonce server_nonce{};
	std::copy_n(challenge_reply.begin() + 5, NonceSize, server_nonce.begin());
	const Proof server_proof = MakeProof(production_token, "server", client_nonce, server_nonce);
	assert(ConstantTimeEqual(server_proof.data(), challenge_reply.data() + 5 + NonceSize, ProofSize));

	const Proof client_proof = MakeProof(production_token, "client", client_nonce, server_nonce);
	std::vector<std::uint8_t> proof{ProofCommand};
	proof.insert(proof.end(), client_proof.begin(), client_proof.end());
	auto proof_packet = Packet(proof);
	const std::vector<std::uint8_t> ordinary{5, 0, 0, 0, 0x0f};
	proof_packet.insert(proof_packet.end(), ordinary.begin(), ordinary.end());
	assert(session.Feed(proof_packet.data(), proof_packet.size()));
	assert(session.Advance(counter));
	assert(session.Verified());
	assert(!session.Ready());
	assert(Drain(session, 2) == std::vector<std::uint8_t>({5, 0, 0, 0, 0}));
	assert(session.Ready());
	assert(session.TakePrefetched() == ordinary);

	ServerSession wrong(production_token);
	assert(wrong.Feed(challenge_packet.data(), challenge_packet.size()));
	assert(wrong.Advance(counter));
	Drain(wrong, 64);
	auto bad_proof = Packet(std::vector<std::uint8_t>(1 + ProofSize, 0));
	bad_proof[4] = ProofCommand;
	assert(wrong.Feed(bad_proof.data(), bad_proof.size()));
	assert(!wrong.Advance(counter));
	assert(wrong.Failed() && !wrong.Verified() && !wrong.Ready());

	ServerSession malformed(token);
	const std::array<std::uint8_t, 4> oversized = {0xff, 0xff, 0xff, 0x7f};
	assert(malformed.Feed(oversized.data(), oversized.size()));
	assert(!malformed.Advance(counter));

	ServerSession bounded(token);
	std::vector<std::uint8_t> excess(BufferLimit + 1, 0);
	assert(!bounded.Feed(excess.data(), excess.size()));

	ServerSession exhausted(token);
	assert(exhausted.Feed(challenge_packet.data(), challenge_packet.size()));
	std::uint64_t maximum = UINT64_MAX;
	assert(!exhausted.Advance(maximum));
}
