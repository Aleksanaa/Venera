// SPDX-License-Identifier: MIT
#include <windows.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cwchar>
#include <string>
#include "native-graphics-rvas.h"

namespace {
constexpr char ods[] = "native-graphics ordinary ODS control";

HMODULE load(const std::wstring& path)
{
    auto mod = LoadLibraryExW(path.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    wchar_t actual[32768];
    DWORD n = mod ? GetModuleFileNameW(mod, actual, ARRAYSIZE(actual)) : 0;
    if (!n || n >= ARRAYSIZE(actual) || _wcsicmp(actual, path.c_str()) != 0) {
        std::fprintf(stderr, "Package image %ls: load/path error %lu\n", path.c_str(), GetLastError());
        std::exit(2);
    }
    std::printf("loaded pid=%lu image=%ls\n", GetCurrentProcessId(), actual);
    return mod;
}

template <typename F> F function(HMODULE mod, size_t rva)
{
    const auto address = reinterpret_cast<const char*>(mod) + rva;
    MEMORY_BASIC_INFORMATION info{};
    if (!VirtualQuery(address, &info, sizeof(info)) || info.AllocationBase != mod ||
        info.Type != MEM_IMAGE || !(info.Protect & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE)))
        std::exit(3);
    return reinterpret_cast<F>(address);
}

int emit(const std::wstring& dir)
{
    auto d11 = load(dir + L"\\venera_d3d11.dll");
    auto open = reinterpret_cast<HRESULT (WINAPI*)(void*)>(GetProcAddress(d11, "OpenAdapter10_2"));
    // DDI entry initializes the package UMD's logger.
    if (!open || open(nullptr) != E_INVALIDARG) return 4;
    function<void (*)(int, const char*, ...)>(d11, umd_log)(1, "%s", "native-graphics package UMD warning");
    OutputDebugStringA(ods);

    std::string shader;
    for (int i = 0; i < 8000; ++i) shader += "mov r0, r1\n";
    function<void (*)(const std::string&)>(d11, dxvk_warn)(shader);

    auto d12 = load(dir + L"\\venera_d3d12.dll");
    function<void (*)(int, int, const char*, const char*, ...)>(d12, vkd_print)(1, 5, "compile", "%s", shader.c_str());
    function<void (*)()>(d12, vkd_flush)();

    auto mesa = load(dir + L"\\venera_vulkan_virtio.dll");
    function<void (*)(const char*)>(mesa, mesa_os)("native-graphics package Mesa OS\n");
    function<void (*)(int, const char*, const char*, ...)>(mesa, mesa_log_rva)
        (2, "MESA-VIRTIO", "%s", "native-graphics package Mesa log");
    std::printf("package-pid=%lu\n", GetCurrentProcessId());
    return 0;
}

int observe(const std::wstring& dir)
{
    wchar_t self[32768];
    auto n = GetModuleFileNameW(nullptr, self, ARRAYSIZE(self));
    if (!n || n >= ARRAYSIZE(self)) return 5;
    std::wstring cmd = L"\"" + std::wstring(self) + L"\" --emit \"" + dir + L"\"";
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
    startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION process{};
    if (!CreateProcessW(self, cmd.data(), nullptr, nullptr, TRUE, DEBUG_ONLY_THIS_PROCESS,
                        nullptr, nullptr, &startup, &process)) return 6;
    CloseHandle(process.hThread);
    unsigned controls = 0;
    DWORD code = 7;
    DEBUG_EVENT event{};
    while (WaitForDebugEvent(&event, 15000)) {
        DWORD status = DBG_CONTINUE;
        if (event.dwDebugEventCode == CREATE_PROCESS_DEBUG_EVENT && event.u.CreateProcessInfo.hFile)
            CloseHandle(event.u.CreateProcessInfo.hFile);
        if (event.dwDebugEventCode == LOAD_DLL_DEBUG_EVENT && event.u.LoadDll.hFile)
            CloseHandle(event.u.LoadDll.hFile);
        if (event.dwDebugEventCode == EXCEPTION_DEBUG_EVENT &&
            event.u.Exception.ExceptionRecord.ExceptionCode != EXCEPTION_BREAKPOINT)
            status = DBG_EXCEPTION_NOT_HANDLED;
        if (event.dwDebugEventCode == OUTPUT_DEBUG_STRING_EVENT) {
            const auto& message = event.u.DebugString;
            char body[sizeof(ods)]{};
            SIZE_T bytes = 0;
            if (!message.fUnicode && message.nDebugStringLength == sizeof(body) &&
                ReadProcessMemory(process.hProcess, message.lpDebugStringData, body, sizeof(body), &bytes) &&
                bytes == sizeof(body) && std::memcmp(body, ods, sizeof(body)) == 0) {
                ++controls;
                std::printf("debugger pid=%lu text=%s\n", event.dwProcessId, ods);
            }
        }
        if (!ContinueDebugEvent(event.dwProcessId, event.dwThreadId, status)) break;
        if (event.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT) {
            code = event.u.ExitProcess.dwExitCode;
            break;
        }
    }
    CloseHandle(process.hProcess);
    if (code) return code;
    return controls == 1 ? 0 : 8;
}
} // namespace

int wmain(int argc, wchar_t** argv)
{
    if (argc == 3 && std::wcscmp(argv[1], L"--emit") == 0) return emit(argv[2]);
    return argc == 2 ? observe(argv[1]) : 1;
}
