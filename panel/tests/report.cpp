// SPDX-License-Identifier: MIT
#include "../panel.h"

#include <cassert>

namespace panel {
std::string narrow(const std::wstring& s)
{
    std::string out(WideCharToMultiByte(CP_UTF8, 0, s.data(), s.size(), nullptr, 0, nullptr, nullptr), '\0');
    WideCharToMultiByte(CP_UTF8, 0, s.data(), s.size(), out.data(), out.size(), nullptr, nullptr);
    return out;
}
}  // namespace panel

int main()
{
    using namespace panel;
    const std::vector<InfoSection> sections = {
        {L"\ue700", L"系统 🚀", {{L"名\"称", L"第一行\nC:\\GPU\t\"ready\"", false, L""},
                                  {L"API", L"Failed", true, L"重试\r\n检查"}}},
        {L"\ue701", L"Empty", {}}};
    assert(reportJson(sections) ==
           "{\"sections\":[{\"title\":\"系统 🚀\",\"rows\":[{\"key\":\"名\\\"称\","
           "\"value\":\"第一行\\nC:\\\\GPU\\t\\\"ready\\\"\",\"bad\":false,\"warning\":\"\"},"
           "{\"key\":\"API\",\"value\":\"Failed\",\"bad\":true,\"warning\":\"重试\\r\\n检查\"}]},"
           "{\"title\":\"Empty\",\"rows\":[]}]}");
    assert(reportJson({}) == "{\"sections\":[]}");
    assert(anyProblem(sections) == true);
    const std::vector<InfoSection> warning = {{L"", L"Driver", {{L"Version", L"1.2", false, L"Restart"}}}};
    assert(anyProblem(warning) == false);
    assert(reportText(warning) ==
           L"Venera system information\r\n\r\nDriver\r\n  Version: 1.2\r\n    Warning: Restart\r\n");
    ProbeResult probe;
    const auto report = describe(SystemInfo{}, &probe);
    assert(report.front().title == L"Driver");
    assert(report.front().rows.front().value == L"Not installed");
    assert(report.front().rows.front().bad == true);
    assert(anyProblem(report) == true);
}
