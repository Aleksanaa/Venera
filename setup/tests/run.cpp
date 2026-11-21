// SPDX-License-Identifier: MIT
#include "../setup.h"

#include <cassert>
#include <cstdarg>
#include <cstdio>
#include <filesystem>

static std::wstring last_command, last_log;
static DWORD launch_error;

static BOOL observed_create_process(LPCWSTR app, LPWSTR command, LPSECURITY_ATTRIBUTES process,
                                   LPSECURITY_ATTRIBUTES thread, BOOL inherit, DWORD flags,
                                   LPVOID environment, LPCWSTR directory, LPSTARTUPINFOW startup,
                                   LPPROCESS_INFORMATION information)
{
    last_command = command;
    wprintf(L"CreateProcessW: %ls\n", command);
    BOOL result = CreateProcessW(app, command, process, thread, inherit, flags, environment, directory,
                                 startup, information);
    launch_error = result ? ERROR_SUCCESS : GetLastError();
    wprintf(L"CreateProcessW result=%d error=%lu\n", result, launch_error);
    SetLastError(launch_error);
    return result;
}

#define CreateProcessW observed_create_process
#include "../run.cpp"
#undef CreateProcessW

std::wstring system_dir()
{
    wchar_t path[MAX_PATH];
    assert(GetSystemDirectoryW(path, MAX_PATH));
    return path;
}

std::wstring format(const wchar_t* pattern, ...)
{
    wchar_t text[16384];
    va_list args;
    va_start(args, pattern);
    int count = vswprintf(text, 16384, pattern, args);
    va_end(args);
    assert(count >= 0);
    return text;
}

void log(const std::wstring& text) { last_log = text; }
void fail(const std::wstring& text, DWORD error) { throw Failure{format(L"%ls: %lu", text.c_str(), error)}; }
void fail(const std::wstring& text) { fail(text, GetLastError()); }

int wmain(int argc, wchar_t** argv)
{
    if (argc == 2 && std::wstring(argv[1]) == L"prepare-history") {
        printf("received prepare-history argc=2\n");
        return 37;
    }
    namespace fs = std::filesystem;
    wchar_t self[32768], temp[MAX_PATH];
    assert(GetModuleFileNameW(nullptr, self, 32768));
    assert(GetTempPathW(MAX_PATH, temp));
    const auto root = fs::path(temp) / (L"Venera launch test " + std::to_wstring(GetCurrentProcessId()));
    fs::create_directories(root);
    const auto child = root / L"venera-debug-guest.exe";
    fs::copy_file(self, child, fs::copy_options::overwrite_existing);
    std::wstring direct_command = L"\"" + child.wstring() + L"\" prepare-history";
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION process;
    assert(CreateProcessW(nullptr, direct_command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW,
                          nullptr, nullptr, &startup, &process));
    assert(WaitForSingleObject(process.hProcess, 10000) == WAIT_OBJECT_0);
    DWORD direct_result = 0;
    assert(GetExitCodeProcess(process.hProcess, &direct_result));
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
    assert(direct_result == 37);
    puts("direct absolute-path control: passed, child returned 37");
    int failed = 0;
    try {
        assert(run(L"schtasks.exe", L"/?") == 0);
        assert(last_command == L"\"" + system_dir() + L"\\schtasks.exe\" /?");
        assert(!last_log.empty());
        puts("system tool basename: passed");
        assert(run(child.wstring(), L"prepare-history") == 37);
        assert(last_command == L"\"" + child.wstring() + L"\" prepare-history");
        assert(last_log.find(L"received prepare-history argc=2") != std::wstring::npos);
        puts("absolute collector path and prepare-history: passed");
    } catch (const Failure& error) {
        wprintf(L"FAILED: %ls\n", error.message.c_str());
        failed = 1;
    }
    fs::remove_all(root);
    return failed;
}
