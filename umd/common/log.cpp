// SPDX-License-Identifier: MIT
#include "log.h"

#include <windows.h>
#include <evntprov.h>

#include <atomic>
#include <cstdarg>
#include <cstdio>
#include <cstring>
#include <mutex>

namespace venera::umd {

namespace {

std::mutex g_lock;
HANDLE g_file = INVALID_HANDLE_VALUE;
std::atomic<bool> g_verbose{false};
const char* g_component = "venera";
bool g_initialized = false;
USHORT g_event_id = 0;
ULONGLONG g_process_created = 0;
ULONGLONG g_sequence = 0;

constexpr GUID kLogProvider = {0x6c5a2378, 0x22b9, 0x4c10, {0xa9, 0x1f, 0x69, 0xe3, 0x4c, 0x28, 0x0b, 0x81}};

// Each pinned UMD owns this registration through process teardown.
struct Registration {
    REGHANDLE handle = 0;
    ~Registration()
    {
        const REGHANDLE registered = handle;
        handle = 0;
        if (registered)
            EventUnregister(registered);
    }
} g_registration;

ULONGLONG filetime_value(const FILETIME& t)
{
    return (static_cast<ULONGLONG>(t.dwHighDateTime) << 32) | t.dwLowDateTime;
}

bool emit(LogLevel level, ULONGLONG timestamp, const char* body)
{
    ++g_sequence;
    if (!g_registration.handle || !g_process_created || !g_event_id)
        return false;
    const EVENT_DESCRIPTOR event{g_event_id, 1, 0, static_cast<UCHAR>(2 + static_cast<int>(level)), 0, 0, 1};
    if (!EventEnabled(g_registration.handle, &event))
        return false;
    EVENT_DATA_DESCRIPTOR data[5];
    EventDataDescCreate(&data[0], &timestamp, sizeof(timestamp));
    EventDataDescCreate(&data[1], &g_process_created, sizeof(g_process_created));
    EventDataDescCreate(&data[2], &g_sequence, sizeof(g_sequence));
    EventDataDescCreate(&data[3], g_component, static_cast<ULONG>(std::strlen(g_component) + 1));
    EventDataDescCreate(&data[4], body, static_cast<ULONG>(std::strlen(body) + 1));
    return EventWrite(g_registration.handle, &event, 5, data) == ERROR_SUCCESS;
}

constexpr const char* kKey = "SOFTWARE\\Venera";

} // namespace

bool setting(const char* name, char* out, unsigned long size)
{
    const DWORD len = GetEnvironmentVariableA(name, out, size);
    if (len > 0 && len < size)
        return true;
    DWORD bytes = size;
    if (RegGetValueA(HKEY_LOCAL_MACHINE, kKey, name, RRF_RT_REG_SZ, nullptr, out, &bytes) == ERROR_SUCCESS)
        return out[0] != 0;
    DWORD value = 0;
    bytes = sizeof(value);
    if (RegGetValueA(HKEY_LOCAL_MACHINE, kKey, name, RRF_RT_REG_DWORD, nullptr, &value, &bytes) != ERROR_SUCCESS)
        return false;
    std::snprintf(out, size, "%lu", value);
    return true;
}

void forward_settings(std::initializer_list<const char*> names)
{
    for (const char* name : names) {
        char value[256];
        if (GetEnvironmentVariableA(name, nullptr, 0) == 0 && setting(name, value, sizeof(value)))
            SetEnvironmentVariableA(name, value);
    }
}

void log_file_beside(const char* name, const char* suffix)
{
    char dir[MAX_PATH], exe[MAX_PATH];
    if (GetEnvironmentVariableA(name, nullptr, 0) || !setting("VENERA_LOG", dir, sizeof(dir)))
        return;
    char* slash = std::strrchr(dir, '\\');
    const DWORD n = GetModuleFileNameA(nullptr, exe, sizeof(exe));
    if (!slash || !n || n >= sizeof(exe))
        return;
    *slash = 0;
    const char* program = std::strrchr(exe, '\\');
    program = program ? program + 1 : exe;
    if (char* dot = std::strrchr(exe, '.'); dot && dot > program)
        *dot = 0;
    char path[MAX_PATH];
    if (std::snprintf(path, sizeof(path), "%s\\%s_%s.log", dir, program, suffix) < int(sizeof(path)))
        SetEnvironmentVariableA(name, path);
}

void log_init(const char* component)
{
    std::lock_guard lock(g_lock);
    if (!g_initialized) {
        g_initialized = true;
        if (std::strcmp(component, "venera_d3d11") == 0) {
            g_component = "venera_d3d11";
            g_event_id = 5;
        } else if (std::strcmp(component, "venera_d3d12") == 0) {
            g_component = "venera_d3d12";
            g_event_id = 6;
        }
        FILETIME created, exited, kernel, user;
        if (GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user))
            g_process_created = filetime_value(created);
        REGHANDLE handle = 0;
        if (g_event_id && EventRegister(&kLogProvider, nullptr, nullptr, &handle) == ERROR_SUCCESS)
            g_registration.handle = handle;
    }
    if (g_file != INVALID_HANDLE_VALUE)
        return;
    char path[MAX_PATH];
    // Append-only access makes each line one atomic append, so processes sharing the file do not cut into each
    // other's lines (the CRT's append mode seeks, then writes). Write-through keeps the log when the VM is killed
    // because the desktop never came up.
    if (setting("VENERA_LOG", path, sizeof(path)))
        g_file = CreateFileA(path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
                             OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH, nullptr);
    if (g_file != INVALID_HANDLE_VALUE)
        log_exceptions();
    char v[8];
    g_verbose = setting("VENERA_VERBOSE", v, sizeof(v)) && v[0] == '1';
}

bool verbose() { return g_verbose.load(std::memory_order_relaxed); }

void logf(LogLevel level, const char* fmt, ...)
{
    static const char* names[] = {"E", "W", "I", "D"};
    char body[1024];
    va_list ap;
    va_start(ap, fmt);
    std::vsnprintf(body, sizeof(body), fmt, ap);
    va_end(ap);

    FILETIME timestamp;
    GetSystemTimePreciseAsFileTime(&timestamp);
    std::lock_guard lock(g_lock);
    // Local wall-clock time, to line up with logs from the host.
    SYSTEMTIME t;
    GetLocalTime(&t);
    char line[1120];
    std::snprintf(line, sizeof(line), "%02u:%02u:%02u.%03u %s[%lu:%lu] %s %s\n", t.wHour, t.wMinute, t.wSecond,
                  t.wMilliseconds, g_component, GetCurrentProcessId(), GetCurrentThreadId(),
                  names[static_cast<int>(level)], body);
    if (!emit(level, filetime_value(timestamp), body))
        OutputDebugStringA(line);
    if (g_file != INVALID_HANDLE_VALUE) {
        DWORD written;
        WriteFile(g_file, line, static_cast<DWORD>(std::strlen(line)), &written, nullptr);
    }
}

} // namespace venera::umd
