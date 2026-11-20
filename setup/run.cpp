// SPDX-License-Identifier: MIT
#include "setup.h"

#include <algorithm>
#include <filesystem>

DWORD run(const std::wstring& exe, const std::wstring& args)
{
    const auto executable = std::filesystem::path(exe).is_absolute() ? exe : system_dir() + L"\\" + exe;
    std::wstring cmd = L"\"" + executable + L"\" " + args;
    SECURITY_ATTRIBUTES sa = {sizeof(sa), nullptr, TRUE};
    HANDLE read_end, write_end;
    if (!CreatePipe(&read_end, &write_end, &sa, 0))
        fail(L"Running " + exe + L" failed");
    SetHandleInformation(read_end, HANDLE_FLAG_INHERIT, 0);
    STARTUPINFOW si = {sizeof(si)};
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = si.hStdError = write_end;
    PROCESS_INFORMATION pi;
    BOOL ok = CreateProcessW(nullptr, cmd.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi);
    DWORD error = GetLastError();
    CloseHandle(write_end);
    if (!ok) {
        CloseHandle(read_end);
        fail(L"Running " + exe + L" failed", error);
    }
    std::string output;
    char buf[4096];
    const auto deadline = GetTickCount64() + 300000;
    bool timed_out = false;
    for (;;) {
        DWORD available = 0, n = 0;
        for (int reads = 0; reads < 16 && PeekNamedPipe(read_end, nullptr, 0, nullptr, &available, nullptr) &&
             available; ++reads) {
            if (!ReadFile(read_end, buf, std::min<DWORD>(sizeof(buf), available), &n, nullptr) || !n)
                break;
            output.append(buf, n);
        }
        if (WaitForSingleObject(pi.hProcess, 0) == WAIT_OBJECT_0 && available == 0)
            break;
        if (GetTickCount64() >= deadline) {
            timed_out = true;
            TerminateProcess(pi.hProcess, ERROR_TIMEOUT);
            WaitForSingleObject(pi.hProcess, 5000);
            break;
        }
        Sleep(50);
    }
    CloseHandle(read_end);
    DWORD code = 1;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    std::wstring wide(MultiByteToWideChar(CP_OEMCP, 0, output.data(), output.size(), nullptr, 0), L'\0');
    MultiByteToWideChar(CP_OEMCP, 0, output.data(), output.size(), wide.data(), wide.size());
    log(format(L"%ls: exit code %lu\n%ls", cmd.c_str(), code, wide.c_str()));
    if (timed_out)
        fail(L"Running " + exe + L" timed out", ERROR_TIMEOUT);
    return code;
}
