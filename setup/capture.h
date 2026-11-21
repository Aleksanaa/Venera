// SPDX-License-Identifier: MIT
#pragma once
#include <evntrace.h>
#include <filesystem>
#include <string>
#include <vector>

namespace capture {
constexpr auto owner = L"VeneraKernelCapture-v1";
constexpr auto primary = L"SYSTEM\\CurrentControlSet\\Control\\WMI\\Autologger\\VeneraKernelLog";
constexpr auto legacy = L"SYSTEM\\CurrentControlSet\\Control\\WMI\\GlobalLogger";
constexpr DWORD process_start_key = 0x00000080;

inline bool owned(HKEY key)
{
    wchar_t value[64]{};
    DWORD size = sizeof(value), type = 0;
    return RegQueryValueExW(key, L"VeneraOwner", nullptr, &type, reinterpret_cast<BYTE*>(value), &size) == 0 &&
        type == REG_SZ && size <= sizeof(value) && value[63] == 0 && wcscmp(value, owner) == 0;
}

inline DWORD set(HKEY key, const wchar_t* name, DWORD value, DWORD type = REG_DWORD)
{
    return RegSetValueExW(key, name, 0, type, reinterpret_cast<const BYTE*>(&value), sizeof(value));
}
inline DWORD text(HKEY key, const wchar_t* name, const std::wstring& value)
{
    return RegSetValueExW(key, name, 0, REG_SZ, reinterpret_cast<const BYTE*>(value.c_str()),
        static_cast<DWORD>((value.size() + 1) * sizeof(wchar_t)));
}

inline DWORD configure(const wchar_t* path, const std::wstring& directory, bool global, HKEY hive = HKEY_LOCAL_MACHINE)
{
    HKEY key = nullptr;
    DWORD disposition = 0;
    DWORD code = RegCreateKeyExW(hive, path, 0, nullptr, 0, KEY_READ | KEY_WRITE,
        nullptr, &key, &disposition);
    if (code) return code;
    if (disposition == REG_OPENED_EXISTING_KEY && !owned(key)) {
        RegCloseKey(key);
        return global ? ERROR_SUCCESS : ERROR_ALREADY_EXISTS;
    }
    const auto save = [&](DWORD result) { if (!code) code = result; };
    save(text(key, L"VeneraOwner", owner));
    save(set(key, L"Start", 0));
    save(text(key, L"FileName", directory + (global ? L"\\legacy-boot.etl" : L"\\kernel-boot.etl")));
    save(set(key, L"LogFileMode", EVENT_TRACE_FILE_MODE_SEQUENTIAL));
    save(set(key, L"ClockType", 2));
    save(set(key, L"MaxFileSize", 32));
    save(set(key, L"FileMax", 3));
    save(set(key, L"BufferSize", 64));
    save(set(key, L"MinimumBuffers", 4));
    save(set(key, L"MaximumBuffers", 16));
    save(set(key, L"FlushTimer", 1));
    if (global) {
        save(set(key, L"EnableKernelFlags", EVENT_TRACE_FLAG_DBGPRINT, REG_BINARY));
    } else {
        save(text(key, L"Guid", L"{6c5a2378-22b9-4c10-a91f-69e34c280b81}"));
        HKEY provider = nullptr;
        save(RegCreateKeyExW(key, L"{6c5a2378-22b9-4c10-a91f-69e34c280b81}", 0, nullptr, 0, KEY_WRITE,
            nullptr, &provider, nullptr));
        if (provider) {
            save(set(provider, L"Enabled", 1));
            save(set(provider, L"EnableProperty", process_start_key));
            save(set(provider, L"EnableLevel", 5));
            ULONGLONG keyword = 1;
            save(RegSetValueExW(provider, L"MatchAnyKeyword", 0, REG_QWORD,
                reinterpret_cast<const BYTE*>(&keyword), sizeof(keyword)));
            RegCloseKey(provider);
        }
        provider = nullptr;
        save(RegCreateKeyExW(key, L"{22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716}", 0, nullptr, 0, KEY_WRITE,
            nullptr, &provider, nullptr));
        if (provider) {
            save(set(provider, L"Enabled", 1));
            save(set(provider, L"EnableLevel", 4));
            save(set(provider, L"EnableProperty", process_start_key));
            ULONGLONG keyword = 0x10;
            save(RegSetValueExW(provider, L"MatchAnyKeyword", 0, REG_QWORD,
                reinterpret_cast<const BYTE*>(&keyword), sizeof(keyword)));
            RegCloseKey(provider);
        }
    }
    if (!code) save(set(key, L"Start", 1));
    if (!code) save(RegFlushKey(key));
    RegCloseKey(key);
    return code;
}

inline DWORD remove(const wchar_t* path, const wchar_t* session, const std::wstring& directory, HKEY hive = HKEY_LOCAL_MACHINE)
{
    HKEY key = nullptr;
    DWORD code = RegOpenKeyExW(hive, path, 0, KEY_READ | KEY_WRITE, &key);
    if (code == ERROR_FILE_NOT_FOUND) return ERROR_SUCCESS;
    if (code) return code;
    if (!owned(key)) { RegCloseKey(key); return ERROR_SUCCESS; }
    code = set(key, L"Start", 0);
    RegCloseKey(key);
    if (code) return code;
    for (const auto name : {session, wcscmp(path, legacy) == 0 ? L"VeneraLegacyLog" : session}) {
        std::vector<ULONGLONG> buffer(8192);
        auto props = reinterpret_cast<EVENT_TRACE_PROPERTIES*>(buffer.data());
        props->Wnode.BufferSize = static_cast<ULONG>(buffer.size() * sizeof(ULONGLONG));
        code = ControlTraceW(0, name, props, EVENT_TRACE_CONTROL_QUERY);
        if (!code) {
            const auto offset = props->LogFileNameOffset;
            if (offset && offset < props->Wnode.BufferSize && offset % sizeof(wchar_t) == 0) {
                auto file = reinterpret_cast<const wchar_t*>(reinterpret_cast<const BYTE*>(props) + offset);
                auto length = (props->Wnode.BufferSize - offset) / sizeof(wchar_t);
                if (wcsnlen(file, length) < length && _wcsicmp(std::filesystem::path(file).parent_path().c_str(), directory.c_str()) == 0)
                    code = ControlTraceW(0, name, props, EVENT_TRACE_CONTROL_STOP);
            }
        }
        if (code != ERROR_SUCCESS && code != ERROR_WMI_INSTANCE_NOT_FOUND) return code;
    }
    return RegDeleteTreeW(hive, path);
}
}
