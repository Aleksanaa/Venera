// SPDX-License-Identifier: MIT
#include <windows.h>
#include <cstdio>
#include <cstdlib>
#include <cwchar>
#include <string>
#include "native-graphics-matrix-rvas.h"

namespace {
[[noreturn]] void fail(const char* operation)
{
    std::fprintf(stderr, "%s failed (Win32 %lu)\n", operation, GetLastError());
    std::exit(2);
}

std::string utf8(const wchar_t* text)
{
    int n = WideCharToMultiByte(CP_UTF8, 0, text, -1, nullptr, 0, nullptr, nullptr);
    if (!n) fail("WideCharToMultiByte size");
    std::string result(n, '\0');
    if (!WideCharToMultiByte(CP_UTF8, 0, text, -1, result.data(), n, nullptr, nullptr))
        fail("WideCharToMultiByte path");
    result.pop_back();
    return result;
}

HMODULE load(const std::wstring& dir, const wchar_t* file)
{
    std::wstring path = dir + L"\\" + file;
    auto mod = LoadLibraryExW(path.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    if (!mod) fail("LoadLibraryExW");
    wchar_t actual[32768];
    DWORD n = GetModuleFileNameW(mod, actual, ARRAYSIZE(actual));
    if (!n || n >= ARRAYSIZE(actual) || _wcsicmp(actual, path.c_str())) fail("GetModuleFileNameW");
    std::printf("loaded pid=%lu bits=%zu image=%s\n", GetCurrentProcessId(), sizeof(void*) * 8, utf8(actual).c_str());
    std::fflush(stdout);
    return mod;
}

template <typename F> F entry(HMODULE mod, size_t rva)
{
    auto address = reinterpret_cast<const char*>(mod) + rva;
    MEMORY_BASIC_INFORMATION info{};
    if (!VirtualQuery(address, &info, sizeof(info)) || info.AllocationBase != mod ||
        info.Type != MEM_IMAGE || !(info.Protect & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE)))
        fail("Mapped logger entry");
    return reinterpret_cast<F>(address);
}

void umd(HMODULE mod, const char* adapter, size_t log, const char* body)
{
    auto open = reinterpret_cast<HRESULT (WINAPI*)(void*)>(GetProcAddress(mod, adapter));
    if (!open || open(nullptr) != E_INVALIDARG) fail(adapter);
    entry<void (*)(int, const char*, ...)>(mod, log)(1, "%s", body);
}

int pair(const std::wstring& dir, const char* body)
{
    if (IsDebuggerPresent()) fail("Standalone pair process");
    auto d11 = load(dir, image_d11);
    umd(d11, "OpenAdapter10_2", umd11_log, body);
    OutputDebugStringA(body);
    std::printf("same-text sent pid=%lu text=%s\n", GetCurrentProcessId(), body);
    // Keep the process alive while the interactive-session DBWIN listener resolves it.
    Sleep(1000);
    return 0;
}

void mesa(HMODULE mod, size_t os_rva, size_t log_rva, const char* tag,
          const char* os_text, const char* log_text)
{
    entry<void (*)(const char*)>(mod, os_rva)(os_text);
    entry<void (*)(int, const char*, const char*, ...)>(mod, log_rva)(2, tag, "%s", log_text);
}

int matrix(const std::wstring& dir)
{
    auto d11 = load(dir, image_d11);
    umd(d11, "OpenAdapter10_2", umd11_log, "graphics bundle D3D11 warning");
    std::string shader;
    for (int i = 0; i < 8000; ++i) shader += "mov r0, r1\n";
    entry<void (*)(const std::string&)>(d11, dxvk_warn)(shader);

    auto d12 = load(dir, image_d12);
    umd(d12, "OpenAdapter12", umd12_log, "graphics bundle D3D12 warning");
    entry<void (*)(int, int, const char*, const char*, ...)>(d12, vkd_print)(1, 5, "compile", "%s", shader.c_str());
    entry<void (*)()>(d12, vkd_flush)();

    mesa(load(dir, image_vk), vk_os, vk_log, "MESA-VIRTIO",
         "graphics bundle Vulkan OS\n", "graphics bundle Vulkan log");
    mesa(load(dir, image_wgl), wgl_os, wgl_log, "MESA",
         "graphics bundle WGL OS\n", "graphics bundle WGL log");
    return 0;
}
} // namespace

int wmain(int argc, wchar_t** argv)
{
    if (argc == 3 && !std::wcscmp(argv[1], L"matrix")) return matrix(argv[2]);
    if (argc == 4 && !std::wcscmp(argv[1], L"pair")) return pair(argv[2], utf8(argv[3]).c_str());
    std::fprintf(stderr, "Usage: native-graphics-matrix matrix PAYLOAD | pair PAYLOAD MARKER\n");
    return 1;
}
