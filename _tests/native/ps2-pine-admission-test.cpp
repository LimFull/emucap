static std::vector<std::uint8_t> AuthPacket(const std::vector<std::uint8_t>& payload)
{
	std::vector<std::uint8_t> packet;
	EmuCap::PineAuth::AppendU32(packet, static_cast<std::uint32_t>(4 + payload.size()));
	packet.insert(packet.end(), payload.begin(), payload.end());
	return packet;
}

static void MarkVerified(PineAuthCandidate& candidate, std::uint64_t& counter)
{
	using namespace EmuCap::PineAuth;
	Nonce client_nonce{};
	for (std::size_t index = 0; index < client_nonce.size(); ++index)
		client_nonce[index] = static_cast<std::uint8_t>(counter + index);
	std::vector<std::uint8_t> challenge{ChallengeCommand};
	challenge.insert(challenge.end(), client_nonce.begin(), client_nonce.end());
	const auto packet = AuthPacket(challenge);
	assert(candidate.auth.Feed(packet.data(), packet.size()));
	assert(candidate.auth.Advance(counter));
	assert(candidate.auth.PendingSize() == 5 + NonceSize + ProofSize);
	Nonce server_nonce{};
	std::copy_n(candidate.auth.PendingData() + 5, NonceSize, server_nonce.begin());
	const std::size_t challenge_reply = candidate.auth.PendingSize();
	assert(candidate.auth.Wrote(challenge_reply));
	const Proof client_proof = MakeProof(ProductionToken, "client", client_nonce, server_nonce);
	std::vector<std::uint8_t> proof{ProofCommand};
	proof.insert(proof.end(), client_proof.begin(), client_proof.end());
	const auto proof_packet = AuthPacket(proof);
	assert(candidate.auth.Feed(proof_packet.data(), proof_packet.size()));
	assert(candidate.auth.Advance(counter));
	assert(candidate.auth.Verified() && !candidate.auth.Ready());
}

int main()
{
	using namespace EmuCap::PineAuth;
	static_assert(PineAcceptBudget == 2);
	(void)&PineAcceptTransient;
	int sockets[2];
	assert(socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) == 0);
	assert(SetPineSocketBlocking(sockets[0], false));
	PineAuthCandidate candidate(sockets[0], ProductionToken);
	Nonce client_nonce{};
	for (std::size_t index = 0; index < client_nonce.size(); ++index)
		client_nonce[index] = static_cast<std::uint8_t>(0x30 + index);
	std::vector<std::uint8_t> challenge{ChallengeCommand};
	challenge.insert(challenge.end(), client_nonce.begin(), client_nonce.end());
	const auto challenge_packet = AuthPacket(challenge);
	std::uint64_t counter = 0;
	std::size_t work = PineWorkBudget;
	for (std::uint8_t byte : challenge_packet)
	{
		assert(write(sockets[1], &byte, 1) == 1);
		assert(ServicePineCandidate(candidate, work, counter) == PineServiceResult::Keep);
	}
	while (candidate.auth.PendingSize() != 0)
		assert(ServicePineCandidate(candidate, work, counter) == PineServiceResult::Keep);
	std::array<std::uint8_t, 5 + NonceSize + ProofSize> challenge_reply{};
	assert(read(sockets[1], challenge_reply.data(), challenge_reply.size()) ==
		static_cast<ssize_t>(challenge_reply.size()));
	Nonce server_nonce{};
	std::copy_n(challenge_reply.begin() + 5, NonceSize, server_nonce.begin());
	const Proof client_proof = MakeProof(ProductionToken, "client", client_nonce, server_nonce);
	std::vector<std::uint8_t> proof{ProofCommand};
	proof.insert(proof.end(), client_proof.begin(), client_proof.end());
	auto proof_packet = AuthPacket(proof);
	const std::vector<std::uint8_t> ordinary{5, 0, 0, 0, 0x0f};
	proof_packet.insert(proof_packet.end(), ordinary.begin(), ordinary.end());
	assert(write(sockets[1], proof_packet.data(), proof_packet.size()) ==
		static_cast<ssize_t>(proof_packet.size()));
	assert(ServicePineCandidate(candidate, work, counter) == PineServiceResult::Keep);
	assert(candidate.auth.Verified());
	assert(ServicePineCandidate(candidate, work, counter) == PineServiceResult::Ready);
	std::array<std::uint8_t, 5> acknowledgement{};
	assert(read(sockets[1], acknowledgement.data(), acknowledgement.size()) ==
		static_cast<ssize_t>(acknowledgement.size()));
	const std::array<std::uint8_t, 5> expected_acknowledgement{5, 0, 0, 0, 0};
	assert(acknowledgement == expected_acknowledgement);
	assert(candidate.auth.TakePrefetched() == ordinary);
	close(sockets[1]);
	ClosePineSocket(candidate.socket);

	std::vector<PineAuthCandidate> saturated;
	saturated.reserve(PineCandidateLimit);
	const auto base = std::chrono::steady_clock::now();
	for (std::size_t index = 0; index < PineCandidateLimit; ++index)
	{
		saturated.emplace_back(InvalidPineSocket, ProductionToken);
		saturated.back().deadline = base + std::chrono::milliseconds(index);
	}
	MarkVerified(saturated[0], counter);
	assert(PineEvictionCandidate(saturated) == 1);
	for (std::size_t index = 1; index < saturated.size(); ++index)
		MarkVerified(saturated[index], counter);
	assert(PineEvictionCandidate(saturated) == saturated.size());

	PineAuthCandidate expired(InvalidPineSocket, ProductionToken);
	expired.deadline = std::chrono::steady_clock::now() - std::chrono::milliseconds(1);
	work = PineWorkBudget;
	assert(ServicePineCandidate(expired, work, counter) == PineServiceResult::Drop);

	int malformed_sockets[2];
	assert(socketpair(AF_UNIX, SOCK_STREAM, 0, malformed_sockets) == 0);
	assert(SetPineSocketBlocking(malformed_sockets[0], false));
	PineAuthCandidate malformed(malformed_sockets[0], ProductionToken);
	const std::array<std::uint8_t, 4> invalid_size{0xff, 0xff, 0xff, 0x7f};
	assert(write(malformed_sockets[1], invalid_size.data(), invalid_size.size()) ==
		static_cast<ssize_t>(invalid_size.size()));
	work = PineWorkBudget;
	assert(ServicePineCandidate(malformed, work, counter) == PineServiceResult::Drop);
	close(malformed_sockets[1]);
	ClosePineSocket(malformed.socket);
}
