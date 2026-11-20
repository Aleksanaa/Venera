#include <windows.h>
#include "../capture.h"
#include <cassert>

DWORD number(HKEY key, const wchar_t* name, DWORD expectedType)
{
    DWORD value = 0, size = sizeof(value), type = 0;
    assert(RegQueryValueExW(key, name, nullptr, &type, reinterpret_cast<BYTE*>(&value), &size) == 0);
    assert(size == 4 && type == expectedType);
    return value;
}
int main()
{
    const auto root = L"Software\\VeneraCaptureTest-" + std::to_wstring(GetCurrentProcessId());
    const auto primary = root + L"\\Primary";
    const auto global = root + L"\\Global";
    const auto foreign = root + L"\\Foreign";
    HKEY key = nullptr;
    assert(RegCreateKeyExW(HKEY_CURRENT_USER, foreign.c_str(), 0, nullptr, 0, KEY_ALL_ACCESS, nullptr, &key, nullptr) == 0);
    assert(capture::set(key, L"Start", 9) == 0);
    assert(capture::configure(foreign.c_str(), L"C:\\capture", true, HKEY_CURRENT_USER) == 0);
    assert(number(key, L"Start", REG_DWORD) == 9);
    assert(capture::owned(key) == false);
    assert(capture::remove(foreign.c_str(), L"Unrelated session", L"C:\\capture", HKEY_CURRENT_USER) == 0);
    assert(number(key, L"Start", REG_DWORD) == 9);
    RegCloseKey(key);
    assert(capture::configure(primary.c_str(), L"C:\\capture", false, HKEY_CURRENT_USER) == 0);
    assert(capture::configure(primary.c_str(), L"C:\\capture", false, HKEY_CURRENT_USER) == 0);
    assert(RegOpenKeyExW(HKEY_CURRENT_USER, primary.c_str(), 0, KEY_READ, &key) == 0);
    assert(capture::owned(key));
    assert(number(key, L"Start", REG_DWORD) == 1);
    assert(number(key, L"FileMax", REG_DWORD) == 3);
    assert(number(key, L"MaxFileSize", REG_DWORD) == 32);
    assert(number(key, L"ClockType", REG_DWORD) == 2);
    RegCloseKey(key);
    const auto process = primary + L"\\{22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716}";
    assert(RegOpenKeyExW(HKEY_CURRENT_USER, process.c_str(), 0, KEY_READ, &key) == 0);
    assert(number(key, L"Enabled", REG_DWORD) == 1);
    assert(number(key, L"EnableLevel", REG_DWORD) == 4);
    assert(number(key, L"EnableProperty", REG_DWORD) == 0x80);
    ULONGLONG keywords = 0;
    DWORD size = sizeof(keywords), type = 0;
    assert(RegQueryValueExW(key, L"MatchAnyKeyword", nullptr, &type, reinterpret_cast<BYTE*>(&keywords), &size) == 0);
    assert(type == REG_QWORD && size == 8 && keywords == 0x10);
    RegCloseKey(key);
    assert(capture::configure(global.c_str(), L"C:\\capture", true, HKEY_CURRENT_USER) == 0);
    assert(RegOpenKeyExW(HKEY_CURRENT_USER, global.c_str(), 0, KEY_READ, &key) == 0);
    assert(number(key, L"EnableKernelFlags", REG_BINARY) == 0x40000);
    RegCloseKey(key);
    assert(RegDeleteTreeW(HKEY_CURRENT_USER, root.c_str()) == 0);
}
