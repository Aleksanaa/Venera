// SPDX-License-Identifier: MIT
#include <windows.h>
#include <evntprov.h>
#include <evntrace.h>
#include <evntcons.h>
#include <algorithm>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <map>
#include <set>
#include <string>
#include <thread>
#include <tuple>
#include <vector>

namespace {
constexpr GUID provider{0x6c5a2378, 0x22b9, 0x4c10, {0xa9, 0x1f, 0x69, 0xe3, 0x4c, 0x28, 0x0b, 0x81}};
struct Process { DWORD pid; ULONGLONG created; unsigned debug_lines; };
std::vector<Process> processes;
std::map<std::pair<DWORD, USHORT>, std::set<ULONGLONG>> sequences;
std::map<std::tuple<DWORD, USHORT, UCHAR>, unsigned> levels;
unsigned errors = 0, events = 0;
ULONGLONG earliest = 0, latest = 0;

ULONGLONG value(const FILETIME& t) { return (ULONGLONG(t.dwHighDateTime) << 32) | t.dwLowDateTime; }
void check(bool ok, const char* message)
{
    if (!ok) { std::fprintf(stderr, "%s\n", message); ++errors; }
}

int emit()
{
    using Emit = void (*)();
    HMODULE modules[]{LoadLibraryA("log-d3d11.dll"), LoadLibraryA("log-d3d12.dll")};
    Emit calls[2]{};
    for (unsigned i = 0; i < 2; ++i) {
        if (!modules[i]) return 2;
        calls[i] = reinterpret_cast<Emit>(GetProcAddress(modules[i], "emit_logs"));
        if (!calls[i]) return 3;
    }
    std::vector<std::thread> threads;
    for (unsigned i = 0; i < 8; ++i)
        threads.emplace_back([&] { calls[0](); calls[1](); });
    for (auto& thread : threads) thread.join();
    for (auto module : modules) FreeLibrary(module);
    return 0;
}

Process run_child()
{
    char exe[MAX_PATH];
    GetModuleFileNameA(nullptr, exe, MAX_PATH);
    std::string command = std::string("\"") + exe + "\" emit";
    STARTUPINFOA startup{};
    startup.cb = sizeof(startup);
    PROCESS_INFORMATION info{};
    if (!CreateProcessA(nullptr, command.data(), nullptr, nullptr, FALSE, DEBUG_ONLY_THIS_PROCESS,
                        nullptr, nullptr, &startup, &info)) {
        check(false, "CreateProcess failed"); return {};
    }
    FILETIME created{}, exited{}, kernel{}, user{};
    check(GetProcessTimes(info.hProcess, &created, &exited, &kernel, &user), "GetProcessTimes failed");
    Process process{info.dwProcessId, value(created), 0};
    bool finished = false;
    while (!finished) {
        DEBUG_EVENT event{};
        if (!WaitForDebugEvent(&event, 30000)) {
            check(false, "debug event timeout"); TerminateProcess(info.hProcess, 4); break;
        }
        DWORD status = DBG_CONTINUE;
        if (event.dwDebugEventCode == OUTPUT_DEBUG_STRING_EVENT) {
            const auto& text = event.u.DebugString;
            std::vector<char> bytes(text.nDebugStringLength + 1, 0);
            SIZE_T read = 0;
            if (!text.fUnicode && ReadProcessMemory(info.hProcess, text.lpDebugStringData,
                                                   bytes.data(), text.nDebugStringLength, &read) &&
                std::strstr(bytes.data(), "shared logf repeated 42"))
                ++process.debug_lines;
        } else if (event.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT) {
            check(event.u.ExitProcess.dwExitCode == 0, "emitter child failed"); finished = true;
        } else if (event.dwDebugEventCode == CREATE_PROCESS_DEBUG_EVENT) {
            if (event.u.CreateProcessInfo.hFile) CloseHandle(event.u.CreateProcessInfo.hFile);
        } else if (event.dwDebugEventCode == LOAD_DLL_DEBUG_EVENT) {
            if (event.u.LoadDll.hFile) CloseHandle(event.u.LoadDll.hFile);
        } else if (event.dwDebugEventCode == EXCEPTION_DEBUG_EVENT &&
                   event.u.Exception.ExceptionRecord.ExceptionCode != EXCEPTION_BREAKPOINT) {
            status = DBG_EXCEPTION_NOT_HANDLED;
        }
        ContinueDebugEvent(event.dwProcessId, event.dwThreadId, status);
    }
    CloseHandle(info.hThread); CloseHandle(info.hProcess);
    return process;
}

void WINAPI receive(EVENT_RECORD* record)
{
    if (std::memcmp(&record->EventHeader.ProviderId, &provider, sizeof(provider))) return;
    const auto& header = record->EventHeader;
    const auto& d = header.EventDescriptor;
    if (d.Id != 5 && d.Id != 6) return;
    auto process = std::find_if(processes.begin(), processes.end(), [&](const Process& p) {
        return p.pid == header.ProcessId;
    });
    if (process == processes.end()) return;
    ++events;
    check(d.Version == 1 && d.Keyword == 1 && d.Channel == 0 && d.Task == 0 && d.Opcode == 0,
          "event descriptor mismatch");
    check(header.ThreadId != 0, "event thread missing");
    check(d.Level >= 2 && d.Level <= 5, "event level mismatch");
    const char* component = d.Id == 5 ? "venera_d3d11" : "venera_d3d12";
    constexpr char message[] = "shared logf repeated 42";
    const size_t expected_size = 24 + std::strlen(component) + 1 + sizeof(message);
    check(record->UserDataLength == expected_size, "payload length mismatch");
    if (record->UserDataLength != expected_size) return;
    ULONGLONG fields[3];
    std::memcpy(fields, record->UserData, sizeof(fields));
    const auto* data = static_cast<const char*>(record->UserData);
    check(fields[0] >= earliest && fields[0] <= latest, "original FILETIME mismatch");
    check(fields[1] == process->created, "process creation FILETIME mismatch");
    check(std::memcmp(data + 24, component, std::strlen(component) + 1) == 0, "component mismatch");
    check(std::memcmp(data + 24 + std::strlen(component) + 1, message, sizeof(message)) == 0,
          "message mismatch");
    check(sequences[{header.ProcessId, d.Id}].insert(fields[2]).second, "duplicate sequence");
    ++levels[{header.ProcessId, d.Id, d.Level}];
}
} // namespace

int main(int argc, char** argv)
{
    if (argc == 2 && std::strcmp(argv[1], "emit") == 0) return emit();
    REGHANDLE registration = 0;
    if (EventRegister(&provider, nullptr, nullptr, &registration) != ERROR_SUCCESS) return 2;
    EVENT_DESCRIPTOR descriptor{5, 1, 0, 2, 0, 0, 1};
    const bool active = EventEnabled(registration, &descriptor);
    EventUnregister(registration);
    const bool capture_only = argc == 2 && std::strcmp(argv[1], "capture") == 0;
    if (active && !capture_only) {
        std::fprintf(stderr, "Run the fallback phase on a test machine with the product capture stopped.\n");
        return 2;
    }
    if (!capture_only) {
        auto fallback = run_child();
        check(fallback.debug_lines == 64, "fallback ODS count mismatch");
    }

    struct Properties { EVENT_TRACE_PROPERTIES p; char name[96]; char file[MAX_PATH]; } properties{};
    std::snprintf(properties.name, sizeof(properties.name), "VeneraUmdLogTest-%lu", GetCurrentProcessId());
    char directory[MAX_PATH];
    GetTempPathA(MAX_PATH, directory);
    std::snprintf(properties.file, sizeof(properties.file), "%svenera-umd-log-%lu.etl", directory, GetCurrentProcessId());
    const std::string file = properties.file;
    auto& p = properties.p;
    p.Wnode.BufferSize = sizeof(properties);
    p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    p.Wnode.ClientContext = 2;
    p.LogFileMode = EVENT_TRACE_FILE_MODE_SEQUENTIAL;
    p.BufferSize = 64;
    p.MinimumBuffers = 4;
    p.MaximumBuffers = 16;
    p.LoggerNameOffset = offsetof(Properties, name);
    p.LogFileNameOffset = offsetof(Properties, file);
    TRACEHANDLE session = 0;
    ULONG result = StartTraceA(&session, properties.name, &p);
    if (result != ERROR_SUCCESS) { std::fprintf(stderr, "StartTrace: %lu\n", result); return 2; }
    result = EnableTraceEx2(session, &provider, EVENT_CONTROL_CODE_ENABLE_PROVIDER, 5, 1, 0, 0, nullptr);
    check(result == ERROR_SUCCESS, "EnableTraceEx2 failed");
    FILETIME time;
    GetSystemTimePreciseAsFileTime(&time); earliest = value(time);
    if (result == ERROR_SUCCESS) {
        processes.push_back(run_child());
        processes.push_back(run_child());
    }
    GetSystemTimePreciseAsFileTime(&time); latest = value(time);
    result = ControlTraceA(session, nullptr, &p, EVENT_TRACE_CONTROL_STOP);
    check(result == ERROR_SUCCESS, "ControlTrace stop failed");
    check(p.EventsLost == 0 && p.LogBuffersLost == 0, "capture lost events");
    EVENT_TRACE_LOGFILEA logfile{};
    logfile.LogFileName = const_cast<char*>(file.c_str());
    logfile.ProcessTraceMode = PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.EventRecordCallback = receive;
    TRACEHANDLE reader = OpenTraceA(&logfile);
    check(reader != INVALID_PROCESSTRACE_HANDLE, "OpenTrace failed");
    if (reader != INVALID_PROCESSTRACE_HANDLE) {
        check(ProcessTrace(&reader, 1, nullptr, nullptr) == ERROR_SUCCESS, "ProcessTrace failed");
        CloseTrace(reader);
    }
    check(events == 128, "ETW event count mismatch");
    for (auto process : processes) {
        check(process.debug_lines == 0, "enabled ODS count mismatch");
        for (USHORT id : {5, 6}) {
            const auto& seen = sequences[{process.pid, id}];
            check(seen.size() == 32 && *seen.begin() == 1 && *seen.rbegin() == 32, "sequence range mismatch");
            for (UCHAR level : {2, 3, 4, 5})
                check(levels[{process.pid, id, level}] == 8, "level count mismatch");
        }
    }
    std::printf("events=%u errors=%u etl=%s\n", events, errors, file.c_str());
    return errors ? 1 : 0;
}
