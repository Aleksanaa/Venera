// SPDX-License-Identifier: MIT
#include "../locale_core.h"
#include "../locale.h"
#include "locale_catalog_fixture.h"
#include <cassert>
#include <stdexcept>

using namespace panel::locale;

namespace panel::locale {
Language testLanguage = Language::English;
const wchar_t* text(unsigned id) { return testLanguage == Language::Chinese ? chinese(id) : english(id); }
const wchar_t* text(unsigned id, Language language) { return language == Language::Chinese ? chinese(id) : english(id); }
Language language() { return testLanguage; }
}

int main()
{
    assert(select(Preference::System, {L"fr-FR", L"zh-CN", L"en-US"}) == Language::Chinese);
    assert(select(Preference::System, {L"en-GB", L"zh-CN"}) == Language::English);
    assert(select(Preference::System, {L"ja-JP"}) == Language::English);
    assert(select(Preference::System, {L"zh-TW", L"zh-Hans-SG"}) == Language::Chinese);
    assert(select(Preference::English, {L"zh-CN"}) == Language::English);
    assert(select(Preference::Chinese, {L"en-US"}) == Language::Chinese);
    assert(parsePreference(L"zh-CN") == Preference::Chinese);
    assert(parsePreference(L"invalid") == Preference::System);
    assert(std::wstring(preferenceName(Preference::English)) == L"en-US");
    assert(std::wstring(preferenceName(Preference::System)) == L"system");
    assert(render(english(IDS_TestSummary), {L"0", L"1", L"12"}) == L"0 passed, 1 failed, 12 not run");
    assert(render(chinese(IDS_TestSummary), {L"0", L"1", L"12"}) == L"通过 0 项，失败 1 项，未运行 12 项");
    assert(render(chinese(IDS_LogDestination), {L"C:\\日志\\venera.log"}) == L"同时将驱动消息写入 C:\\日志\\venera.log");
    assert(render(L"{1}：{0}", {L"设备", L"状态"}) == L"状态：设备");
    assert(render(L"{0}", {L"{1}"}) == L"{1}");
    Message failure(IDS_UpdateCreateFile, {LR"(C:\日志\venera.log)", L"error 5"});
    assert(failure.resolve() == LR"(Could not create C:\日志\venera.log: error 5)");
    testLanguage = Language::Chinese;
    assert(failure.resolve() == LR"(无法创建 C:\日志\venera.log：error 5)");
    assert(failure.resolve(Language::English) == LR"(Could not create C:\日志\venera.log: error 5)");
    Message crash(IDS_TestCrashed, {L"0xc0000005"});
    crash.diagnostic = L": ACCESS VIOLATION in mesa.dll+0x123";
    assert(crash.resolve() == L"已崩溃（0xc0000005）: ACCESS VIOLATION in mesa.dll+0x123");
    testLanguage = Language::English;
    assert(crash.resolve() == L"Crashed (0xc0000005): ACCESS VIOLATION in mesa.dll+0x123");
    Message socketSetup(IDS_SocketSetupFailed, {L"1223"});
    assert(socketSetup.resolve() == L"VirtIO socket driver setup could not start (1223).");
    testLanguage = Language::Chinese;
    assert(socketSetup.resolve() == L"无法启动 VirtIO 套接字驱动安装程序（1223）。");
    assert(socketSetup.resolve(Language::English) == L"VirtIO socket driver setup could not start (1223).");
    assert(render(chinese(IDS_ReportDisplayValue), {L"1920", L"1080", L"60", L"150"}) == L"1920 × 1080，60 Hz，缩放 150%");
    assert(Message(L"raw {0}: 0x887a0004").resolve() == L"raw {0}: 0x887a0004");
    assert(Message{}.empty());
    bool missing = false, extra = false, malformed = false;
    try { render(L"{1}", {L"x"}); } catch (const std::invalid_argument&) { missing = true; }
    try { render(L"{0}", {L"x", L"y"}); } catch (const std::invalid_argument&) { extra = true; }
    try { render(L"{name}", {L"x"}); } catch (const std::invalid_argument&) { malformed = true; }
    assert(missing && extra && malformed);
}
