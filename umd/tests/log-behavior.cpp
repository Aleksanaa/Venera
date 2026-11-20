// SPDX-License-Identifier: MIT
#include "../common/log.h"
#include <windows.h>
#include <evntprov.h>
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <set>
#include <string>
#include <thread>
#include <vector>

namespace {
struct Event { EVENT_DESCRIPTOR descriptor; std::vector<unsigned char> bytes; };
std::vector<Event> events;
std::vector<std::string> ods, files;
bool enabled = true, registration_ok = true, process_time_ok = true;
bool verbose_setting = true;
bool file_available = true;
ULONG write_result = 0;
unsigned registrations = 0;
void check(bool ok, const char* message)
{
    if (!ok) { std::fprintf(stderr, "%s\n", message); std::exit(1); }
}
uint64_t number(const Event& event, size_t offset)
{
    uint64_t value = 0;
    for (unsigned i = 0; i < 8; ++i) value |= uint64_t(event.bytes.at(offset + i)) << (i * 8);
    return value;
}
}

DWORD GetEnvironmentVariableA(const char* name, char* out, DWORD size)
{
    const char* value = std::strcmp(name, "VENERA_LOG") == 0 ? "test.log" :
                        std::strcmp(name, "VENERA_VERBOSE") == 0 ? (verbose_setting ? "1" : "0") : "";
    const size_t length = std::strlen(value);
    if (length && size > length) std::memcpy(out, value, length + 1);
    return length;
}
DWORD RegGetValueA(HANDLE, const char*, const char*, DWORD, DWORD*, void*, DWORD*) { return 2; }
BOOL SetEnvironmentVariableA(const char*, const char*) { return 1; }
DWORD GetModuleFileNameA(HANDLE, char*, DWORD) { return 0; }
HANDLE GetCurrentProcess() { return reinterpret_cast<HANDLE>(1); }
BOOL GetProcessTimes(HANDLE, FILETIME* created, FILETIME*, FILETIME*, FILETIME*)
{
    *created = {0x89abcdef, 0x01234567}; return process_time_ok;
}
HANDLE CreateFileA(const char*, DWORD, DWORD, void*, DWORD, DWORD, HANDLE)
{
    return file_available ? GetCurrentProcess() : INVALID_HANDLE_VALUE;
}
BOOL WriteFile(HANDLE, const void* bytes, DWORD size, DWORD* written, void*)
{
    files.emplace_back(static_cast<const char*>(bytes), size); *written = size; return 1;
}
void GetSystemTimePreciseAsFileTime(FILETIME* time) { *time = {0x76543210, 0x01dc1234}; }
void GetLocalTime(SYSTEMTIME* time) { *time = {2026, 11, 2, 17, 12, 34, 56, 789}; }
DWORD GetCurrentProcessId() { return 123; }
DWORD GetCurrentThreadId() { return 456; }
void OutputDebugStringA(const char* line) { ods.emplace_back(line); }
ULONG EventRegister(const GUID* id, void*, void*, REGHANDLE* handle)
{
    check(id->a == 0x6c5a2378 && id->b == 0x22b9 && id->c == 0x4c10, "product provider");
    constexpr uint8_t tail[]{0xa9, 0x1f, 0x69, 0xe3, 0x4c, 0x28, 0x0b, 0x81};
    check(std::memcmp(id->d, tail, 8) == 0, "product provider tail");
    ++registrations; *handle = registration_ok ? 99 : 0; return registration_ok ? 0 : 5;
}
ULONG EventUnregister(REGHANDLE) { return 0; }
BOOL EventEnabled(REGHANDLE, const EVENT_DESCRIPTOR*) { return enabled; }
ULONG EventWrite(REGHANDLE, const EVENT_DESCRIPTOR* descriptor, ULONG count, EVENT_DATA_DESCRIPTOR* data)
{
    Event event{*descriptor, {}};
    for (ULONG i = 0; i < count; ++i) {
        auto* begin = static_cast<const unsigned char*>(data[i].data);
        event.bytes.insert(event.bytes.end(), begin, begin + data[i].size);
    }
    events.push_back(std::move(event));
    return write_result;
}
namespace venera::umd { void log_exceptions() {} }

int main(int argc, char** argv)
{
    using namespace venera::umd;
    if (argc == 2) {
        if (std::strcmp(argv[1], "file-retry") == 0) {
            file_available = false;
            log_init("venera_d3d11");
            logf(LogLevel::info, "first");
            file_available = true;
            log_init("venera_d3d11");
            logf(LogLevel::info, "second");
            check(registrations == 1 && events.size() == 2 && number(events.back(), 16) == 2,
                  "registration and sequence through file retry");
            check(files == std::vector<std::string>{"12:34:56.789 venera_d3d11[123:456] I second\n"},
                  "file output after retry");
            return 0;
        }
        if (std::strcmp(argv[1], "verbose-off") == 0) {
            verbose_setting = false;
            log_init("venera_d3d11");
            VENERA_VERBOSE("high-volume %d", 9);
            logf(LogLevel::info, "ordinary %d", 7);
            check(events.size() == 1 && events[0].descriptor.Level == 4 && number(events[0], 16) == 1,
                  "ordinary logs with verbose setting off");
            check(files == std::vector<std::string>{"12:34:56.789 venera_d3d11[123:456] I ordinary 7\n"},
                  "ordinary log contents with verbose setting off");
            return 0;
        }
        registration_ok = std::strcmp(argv[1], "registration") != 0;
        process_time_ok = std::strcmp(argv[1], "process-time") != 0;
        log_init("venera_d3d11");
        logf(LogLevel::error, "fallback %d", 7);
        check(files == std::vector<std::string>{"12:34:56.789 venera_d3d11[123:456] E fallback 7\n"},
              "file content on initialization failure");
        check(ods.empty() && events.empty(), "initialization failure output");
        return 0;
    }
    check(argc == 3, "component and ID required");
    const char* component = argv[1];
    const int id = std::atoi(argv[2]);
    std::vector<std::thread> threads;
    for (unsigned i = 0; i < 16; ++i) threads.emplace_back([&] {
        log_init(component);
        for (auto level : {LogLevel::error, LogLevel::warn, LogLevel::info, LogLevel::debug})
            logf(level, "repeat %d", 42);
    });
    for (auto& thread : threads) thread.join();
    check(registrations == 1 && events.size() == 64 && files.size() == 64, "parallel initialization and output");
    check(ods.empty(), "ETW routes enabled log calls");
    std::set<uint64_t> sequence;
    unsigned levels[6]{};
    for (const auto& event : events) {
        const auto& d = event.descriptor;
        check(d.Id == id && d.Version == 1 && d.Keyword == 1 && d.Channel == 0 && d.Task == 0 && d.Opcode == 0,
              "event descriptor");
        check(d.Level >= 2 && d.Level <= 5, "severity range"); ++levels[d.Level];
        check(number(event, 0) == 0x01dc123476543210ULL, "original FILETIME");
        check(number(event, 8) == 0x0123456789abcdefULL, "process instance FILETIME");
        check(sequence.insert(number(event, 16)).second, "repeated messages retain identity");
        std::string expected(component); expected.push_back('\0'); expected += "repeat 42"; expected.push_back('\0');
        check(event.bytes.size() == 24 + expected.size() &&
              std::memcmp(event.bytes.data() + 24, expected.data(), expected.size()) == 0, "payload strings and size");
    }
    check(*sequence.begin() == 1 && *sequence.rbegin() == 64, "parallel sequence range");
    for (unsigned i = 2; i <= 5; ++i) check(levels[i] == 16, "severity counts");
    check(verbose(), "verbose setting");
    VENERA_VERBOSE("verbose %d", 9);
    check(number(events.back(), 16) == 65 && events.back().descriptor.Level == 5, "verbose event");
    enabled = false;
    logf(LogLevel::warn, "fallback %d", 7);
    const std::string expected = std::string("12:34:56.789 ") + component + "[123:456] W fallback 7\n";
    check(ods.empty() && files.size() == 66 && files.back() == expected, "file with capture disabled");
    enabled = true; write_result = 8;
    logf(LogLevel::warn, "fallback %d", 7);
    check(ods.empty() && files.size() == 67 && files.back() == expected, "file after EventWrite error");
    write_result = 0;
    logf(LogLevel::info, "%s", std::string(2000, 'x').c_str());
    const auto& last = events.back();
    check(number(last, 16) == 68 && last.bytes.size() == 24 + std::strlen(component) + 1 + 1024,
          "bounded message and continued sequence");
    check(last.bytes.back() == 0 && last.bytes[last.bytes.size() - 2] == 'x', "bounded message terminator");
    std::puts("logf behavior passed");
}
