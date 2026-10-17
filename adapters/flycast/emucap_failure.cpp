#include "emucap_failure.h"
#include "emucap_native_failure.h"

#include <algorithm>
#include <cstdio>
#include <sstream>

namespace {

std::string json_escape(const std::string& value)
{
	std::string escaped;
	escaped.reserve(value.size());
	for (unsigned char ch : value)
	{
		switch (ch)
		{
		case '"': escaped += "\\\""; break;
		case '\\': escaped += "\\\\"; break;
		case '\n': escaped += "\\n"; break;
		case '\r': escaped += "\\r"; break;
		case '\t': escaped += "\\t"; break;
		default:
			if (ch < 0x20)
			{
				char buffer[8];
				std::snprintf(buffer, sizeof(buffer), "\\u%04x", ch);
				escaped += buffer;
			}
			else
				escaped += static_cast<char>(ch);
		}
	}
	return escaped;
}

std::string bounded_text(const std::string& value, std::size_t cap, bool& truncated)
{
	if (value.size() <= cap)
		return value;
	truncated = true;
	std::size_t cut = cap;
	// Runtime paths are UTF-8 on supported launchers. Do not cut inside a multibyte code point and
	// turn the entire otherwise-durable JSON artifact into invalid UTF-8.
	while (cut > 0 && (static_cast<unsigned char>(value[cut]) & 0xC0) == 0x80)
		--cut;
	return value.substr(0, cut);
}

} // namespace

std::string emucap_failure_json(const EmucapSh4FailureSnapshot& snapshot)
{
	bool truncated = false;
	const std::string launch_id = bounded_text(snapshot.launch_id, 128, truncated);
	const std::string emulator_build = bounded_text(snapshot.emulator_build, 128, truncated);
	const std::string content = bounded_text(snapshot.content, 1024, truncated);
	const std::string reason = bounded_text(snapshot.reason, 512, truncated);
	std::ostringstream out;
	out << "{\"schema_version\":1"
		<< ",\"launch_id\":\"" << json_escape(launch_id) << '"'
		<< ",\"adapter\":\"flycast-native\""
		<< ",\"emulator_build\":\"" << json_escape(emulator_build) << '"'
		<< ",\"content\":\"" << json_escape(content) << '"'
		<< ",\"kind\":\"sh4_fatal\""
		<< ",\"reason\":\"" << json_escape(reason) << '"'
		<< ",\"observed_at_unix_ms\":" << snapshot.observed_at_unix_ms
		<< ",\"frame\":" << snapshot.frame
		<< ",\"epc\":" << snapshot.epc
		<< ",\"incoming_event\":" << snapshot.incoming_event
		<< ",\"existing_expevt\":" << snapshot.existing_expevt
		<< ",\"existing_intevt\":" << snapshot.existing_intevt
		<< ",\"tea\":" << snapshot.tea
		<< ",\"trace_scope\":\"interpreter\""
		<< ",\"registers\":{";
	for (std::size_t i = 0; i < snapshot.r.size(); ++i)
		out << (i == 0 ? "" : ",") << "\"r" << i << "\":" << snapshot.r[i];
	for (std::size_t i = 0; i < snapshot.r_bank.size(); ++i)
		out << ",\"r_bank" << i << "\":" << snapshot.r_bank[i];
	out << ",\"pc\":" << snapshot.pc
		<< ",\"pr\":" << snapshot.pr
		<< ",\"gbr\":" << snapshot.gbr
		<< ",\"vbr\":" << snapshot.vbr
		<< ",\"mach\":" << snapshot.mach
		<< ",\"macl\":" << snapshot.macl
		<< ",\"sr\":" << snapshot.sr
		<< ",\"ssr\":" << snapshot.ssr
		<< ",\"spc\":" << snapshot.spc
		<< ",\"sgr\":" << snapshot.sgr
		<< ",\"dbr\":" << snapshot.dbr
		<< ",\"fpul\":" << snapshot.fpul
		<< ",\"fpscr\":" << snapshot.fpscr
		<< "},\"pc_ring\":[";
	const std::size_t count = std::min(snapshot.pc_ring_count, EMUCAP_CRASH_PC_CAP);
	const std::size_t head = snapshot.pc_ring_head & (EMUCAP_CRASH_PC_CAP - 1);
	for (std::size_t i = 0; i < count; ++i)
	{
		const std::size_t index = (head + EMUCAP_CRASH_PC_CAP - count + i)
			& (EMUCAP_CRASH_PC_CAP - 1);
		out << (i == 0 ? "" : ",") << snapshot.pc_ring[index];
	}
	out << "],\"truncated\":" << (truncated ? "true" : "false") << '}';
	std::string json = out.str();
	if (json.size() > EMUCAP_FAILURE_FILE_CAP && snapshot.pc_ring_count > 0)
	{
		// The fixed-size schema is normally below 16 KiB. Keep exact registers even if a future
		// schema addition accidentally exceeds the contract, and explicitly mark the ring omitted.
		// The count guard also makes this fallback single-shot if future fixed fields grow too large.
		EmucapSh4FailureSnapshot reduced = snapshot;
		reduced.launch_id = launch_id;
		reduced.emulator_build = emulator_build;
		reduced.content = content;
		reduced.reason = reason;
		reduced.pc_ring_count = 0;
		json = emucap_failure_json(reduced);
		const std::string marker = "\"truncated\":false";
		if (const std::size_t pos = json.rfind(marker); pos != std::string::npos)
			json.replace(pos, marker.size(), "\"truncated\":true");
	}
	return json;
}

bool emucap_write_failure_atomic(
	const std::string& path,
	const std::string& json,
	std::string* error)
{
	return emucap_write_native_failure_atomic(path, json, error);
}

#ifdef _WIN32
bool emucap_write_failure_atomic_wide(
	const std::wstring& path,
	const std::string& json,
	std::string* error)
{
	return emucap_write_native_failure_atomic_wide(path, json, error);
}
#endif
