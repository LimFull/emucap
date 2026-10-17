// Actual console tokenizer/dispatcher; handlers record parameter boundaries.
#include <algorithm>
#include <cassert>
#include <cctype>
#include <cstdint>
#include <functional>
#include <set>
#include <string>
#include <string_view>
#include <vector>
using u8 = uint8_t;
using u16 = uint16_t;
using u32 = uint32_t;
constexpr u32 CMDFLAG_KEEP_QUOTES = 1;
struct expression_error { operator u16() const { return 0; } };
struct parsed_expression {
    parsed_expression(int, std::string_view) { assert(false); }
    void execute() { assert(false); }
};
static std::string strmakelower(std::string_view value) {
    std::string s(value);
    std::transform(s.begin(), s.end(), s.begin(), [](unsigned char c) { return std::tolower(c); });
    return s;
}
#include "mame-cmderr.inc"
class debugger_console {
public:
    struct command {
        std::string command;
        u32 flags;
        size_t minparams, maxparams;
        std::function<void(const std::vector<std::string_view>&)> handler;
    };
    struct compare {
        using is_transparent = void;
        bool operator()(const command& a, const command& b) const { return a.command < b.command; }
        bool operator()(const command& a, const char* b) const { return a.command < b; }
        bool operator()(const char* a, const command& b) const { return a < b.command; }
    };
    std::set<command, compare> m_commandlist;
    std::vector<std::vector<std::string>> calls;
    debugger_console() {
        for(const auto& name : {"bpset", "wp", "rpset"}) {
            const size_t count = name == std::string("wp") ? 3 : 1;
            m_commandlist.insert({name, 0, count, count + 2, [&, name](const std::vector<std::string_view>& params) {
                std::vector<std::string> copy{name};
                for(auto param : params) copy.emplace_back(param);
                calls.push_back(std::move(copy));
            }});
        }
    }
    static std::string_view trim_parameter(std::string_view, bool);
    CMDERR internal_execute_command(bool, std::vector<std::string_view>&);
    CMDERR internal_parse_command(std::string_view, bool);
    CMDERR execute_command_with_parameters(std::string_view, const std::vector<std::string>&);
    int visible_symtable() { return 0; }
    static std::string cmderr_to_string(CMDERR) { return "error"; }
    void printf(const char*, const std::string&) {}
};
#include "mame-console-functions.inc"

int main() {
    debugger_console console;
    auto invoke = [&](const std::string& name, const std::vector<std::string>& params) {
#ifdef HAS_STRUCTURED_COMMAND
        return console.execute_command_with_parameters(name, params);
#else
        std::string text = name + " ";
        for(size_t i = 0; i < params.size(); i++) text += (i ? "," : "") + params[i];
        return console.internal_parse_command(text, true);
#endif
    };
    for(const auto& name : {"bpset", "wp", "rpset"}) {
        for(const auto& condition : {"1; bpset 200", "1,{bpset 200;g}", "max(esp,eax) == 1",
                                    "(esp=1, eax=2)", "esp == 'a'", "{esp < 10}"}) {
            std::vector<std::string> params;
            if(name == std::string("bpset")) params = {"100"};
            if(name == std::string("wp")) params = {"100", "4", "rw"};
            params.emplace_back(condition);
            console.calls.clear();
            assert(invoke(name, params).error_class() == CMDERR::NONE);
            assert(console.calls.size() == 1);
            assert(console.calls[0].size() == params.size() + 1);
            assert(console.calls[0].back() == debugger_console::trim_parameter(condition, false));
        }
    }
    // Existing command lookup, optional conditions and native parameter errors remain intact.
    console.calls.clear();
    assert(invoke("bpset", {"100"}).error_class() == CMDERR::NONE);
    assert(invoke("wp", {"100", "4", "w"}).error_class() == CMDERR::NONE);
    assert(invoke("rpset", {"esp < 10"}).error_class() == CMDERR::NONE);
    assert(console.calls.size() == 3);
    assert(invoke("unknown", {"1"}).error_class() == CMDERR::UNKNOWN_COMMAND);
    assert(invoke("wp", {"100"}).error_class() == CMDERR::NOT_ENOUGH_PARAMS);
    assert(console.calls.size() == 3);
    // Interactive console syntax is unchanged; only the structured path is single-command.
    console.calls.clear();
    assert(console.internal_parse_command("bpset 100,1; bpset 200", true).error_class() == CMDERR::NONE);
    assert(console.calls.size() == 2);
}
