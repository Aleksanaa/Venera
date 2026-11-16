// SPDX-License-Identifier: MIT
#include "../panel.h"
#include "../debugjson.h"

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
    SystemInfo startupInfo;
    startupInfo.memory = 8ull << 30;
    startupInfo.usableMemory = 7ull << 30;
    startupInfo.displays = {{1920, 1080, 60, 150}};
    const auto startup = describe(startupInfo, nullptr);
    assert(startup[2].rows[0].value == L"8 GB (7 GB usable)");
    assert(startup[2].rows[0].displayValue.resolve(locale::Language::Chinese) == L"8 GB（可用 7 GB）");
    assert(startup[2].rows[2].value == L"1920 × 1080 at 60 Hz, 150% scale");
    assert(startup[2].rows[2].displayValue.resolve(locale::Language::Chinese) == L"1920 × 1080，60 Hz，缩放 150%");
    assert(reportText(startup).find(L"  Display: 1920 × 1080 at 60 Hz, 150% scale\r\n") != std::wstring::npos);
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
    assert(reportJson({report.front()}) ==
           "{\"sections\":[{\"title\":\"Driver\",\"rows\":[{\"key\":\"Version\",\"value\":\"Not installed\",\"bad\":true,\"warning\":\"\",\"key_message\":{\"id\":\"report.version\",\"args\":[]},\"value_message\":{\"id\":\"report.notInstalled\",\"args\":[]}},"
           "{\"key\":\"Device\",\"value\":\"No VirtIO GPU found\",\"bad\":true,\"warning\":\"\",\"key_message\":{\"id\":\"report.device\",\"args\":[]},\"value_message\":{\"id\":\"report.noDevice\",\"args\":[]}},"
           "{\"key\":\"Test signing\",\"value\":\"Off, so Windows does not load the driver\",\"bad\":true,\"warning\":\"\",\"key_message\":{\"id\":\"report.testSigning\",\"args\":[]},\"value_message\":{\"id\":\"report.signingOff\",\"args\":[]}}],\"title_message\":{\"id\":\"report.driver\",\"args\":[]}}]}");
    assert(std::wstring(locale::text(report.front().titleId, locale::Language::Chinese)) == L"驱动");
    assert(report.front().rows.front().displayKey.resolve(locale::Language::Chinese) == L"版本");
    assert(report.front().rows.front().displayValue.resolve(locale::Language::Chinese) == L"尚未安装");
    assert(std::wstring(locale::text(IDS_Copy, locale::Language::English)) == L"Copy");
    assert(std::wstring(locale::text(IDS_Copy, locale::Language::Chinese)) == L"复制");
    SystemInfo info;
    info.deviceFound = true;
    info.driverDesc = L"VirtIO GPU (Venus)";
    info.memory = 8ull << 30;
    info.usableMemory = 7ull << 30;
    info.displays = {{1920, 1080, 60, 150}};
    const auto populated = describe(info, &probe);
    assert(populated.front().rows[1].value == L"VirtIO GPU (Venus)");
    assert(populated.front().rows[1].displayValue.resolve(locale::Language::Chinese) == L"VirtIO GPU (Venus)");
    assert(populated[2].rows[0].value == L"8 GB (7 GB usable)");
    assert(populated[2].rows[0].displayValue.resolve(locale::Language::Chinese) == L"8 GB（可用 7 GB）");
    assert(populated[2].rows[2].value == L"1920 × 1080 at 60 Hz, 150% scale");
    assert(populated[2].rows[2].displayValue.resolve(locale::Language::Chinese) == L"1920 × 1080，60 Hz，缩放 150%");
    const auto json = debugjson::Json::parse(reportJson(populated));
    assert(json["sections"][2]["rows"][0]["value_message"] ==
           debugjson::Json({{"id", "report.memoryValue"}, {"args", {"8 GB", "7 GB"}}}));
    assert(json["sections"][2]["rows"][2]["value_message"] ==
           debugjson::Json({{"id", "report.displayValue"}, {"args", {"1920", "1080", "60", "150"}}}));
    auto suffixed = populated;
    suffixed[0].rows[0].displayWarning = {IDS_ReportApiFailed, {L"诊断 🚀"}};
    suffixed[0].rows[0].displayWarning.diagnostic = L"\nC:\\GPU 原始后缀";
    suffixed[0].rows[0].warning = suffixed[0].rows[0].displayWarning.resolve(locale::Language::English);
    const auto diagnostic = debugjson::Json::parse(reportJson(suffixed));
    assert(diagnostic["sections"][0]["rows"][0]["warning"] == "Failed (诊断 🚀)\nC:\\GPU 原始后缀");
    assert(diagnostic["sections"][0]["rows"][0]["warning_message"] ==
           debugjson::Json({{"id", "report.apiFailed"}, {"args", {"诊断 🚀"}}, {"diagnostic", "\nC:\\GPU 原始后缀"}}));
    const auto englishJson = reportJson(describe(info, &probe));
    ProbeResult independent = {{"vk.status", L"no loader"}, {"venus.status", L"ok"},
                               {"venus.device", L"Virtio-GPU Venus (AMD Radeon 8060S (RADV STRIX_HALO))"},
                               {"venus.host_driver_info", L"Mesa 25.2.4"}};
    const auto runtimeMissing = describe(info, &independent);
    assert(runtimeMissing[1].rows[0].value == L"AMD Radeon 8060S");
    assert(runtimeMissing[1].rows[0].bad == false);
    assert(runtimeMissing[1].rows[1].value == L"RADV STRIX_HALO, Mesa 25.2.4");
    assert(runtimeMissing[3].rows[3].value == L"The Vulkan Runtime is not installed");
    assert(runtimeMissing[3].rows[3].bad == true);
    assert(runtimeMissing[3].rows[3].displayValue.resolve(locale::Language::Chinese) == L"尚未安装 Vulkan 运行时");
    const auto missingJson = debugjson::Json::parse(reportJson(runtimeMissing));
    assert(missingJson["sections"][1]["rows"][0]["value"] == "AMD Radeon 8060S");
    assert(missingJson["sections"][3]["rows"][3]["value_message"] ==
           debugjson::Json({{"id", "report.noLoader"}, {"args", debugjson::Json::array()}}));
    independent["venus.status"] = L"loading Venus: Windows error 126";
    independent["vk.device"] = L"VirtIO GPU";
    const auto venusFailed = describe(info, &independent);
    assert(venusFailed[1].rows[0].value == L"GPU query failed (loading Venus: Windows error 126)");
    assert(venusFailed[1].rows[0].bad == true);
    assert(venusFailed[1].rows[0].displayValue.resolve(locale::Language::Chinese) ==
           L"GPU 查询失败（loading Venus: Windows error 126）");
    assert(debugjson::Json::parse(reportJson(venusFailed))["sections"][1]["rows"][0]["value_message"] ==
           debugjson::Json({{"id", "report.gpuFailed"}, {"args", {"loading Venus: Windows error 126"}}}));
    independent["venus.status"] = L"checking";
    independent["timeout"] = L"1";
    const auto venusTimeout = describe(info, &independent);
    assert(venusTimeout[1].rows[0].value == L"The check stopped responding");
    assert(venusTimeout[1].rows[0].bad == true);
    independent.erase("timeout");
    assert(describe(info, &independent)[1].rows[0].value == L"The check crashed");
    independent["venus.status"] = L"no device";
    assert(describe(info, &independent)[1].rows[0].value == L"No Venus device");
    ProbeResult complete = {{"vk.status", L"ok"},
                            {"vk.device", L"Virtio-GPU Venus (AMD Radeon 8060S (RADV STRIX_HALO))"},
                            {"vk.api", L"1.3.0"}, {"vk.driver", L"venus"}, {"vk.driver_info", L"Mesa 25.2.4"},
                            {"vk.heap.0", L"8589934592 d"},
                            {"d3d12.status", L"ok"}, {"d3d12.level", L"12_1"}, {"d3d12.shader_model", L"6.6"},
                            {"d3d11.status", L"ok"}, {"d3d11.level", L"11_1"},
                            {"gl.status", L"ok"}, {"gl.version", L"4.6 Mesa 25.2.4"}, {"gl.renderer", L"zink"}};
    const auto legacyComplete = describe(info, &complete);
    assert(legacyComplete[1].rows[0].value == L"AMD Radeon 8060S");
    assert(legacyComplete[3].rows[3].value == L"1.3.0 (venus, Mesa 25.2.4)");
    assert(legacyComplete[3].rows[3].bad == false);
    for (const auto& [key, value] : complete)
        if (key.compare(0, 3, "vk.") == 0)
            independent["venus." + key.substr(3)] = value;
    independent["vk.status"] = L"no loader";
    const auto directComplete = describe(info, &independent);
    assert(directComplete[1].rows[0].value == L"AMD Radeon 8060S");
    assert(directComplete[4].rows[0].value == L"8 GB, device-local");
    for (const auto& [key, value] : independent)
        if (key.compare(0, 6, "venus.") == 0)
            complete[key] = value;
    const auto fullReport = describe(info, &complete);
    assert(fullReport[1].rows[0].value == L"AMD Radeon 8060S");
    assert(fullReport[3].rows[3].value == L"1.3.0 (venus, Mesa 25.2.4)");
    assert(fullReport[3].rows[3].bad == false);
    locale::change(locale::Preference::Chinese);
    assert(reportJson(describe(info, &probe)) == englishJson);
}
