// SPDX-License-Identifier: MIT
// Records a performance trace in the guest: our UMDs' and Venus ICD's timeline events (the Venera ETW provider,
// umd/common/trace.h) together with dxgkrnl's and DWM's, all timestamped with the CPU's TSC, which KVM offsets by a
// constant from the host's: tests/perf/vtrace.py lines them up with a host trace of the same time. Run elevated.
//   venera-trace record SECONDS DIR   start, wait, stop
//   venera-trace start DIR            start tracing into DIR\trace.etl
//   venera-trace stop                 stop, and decode into events.jsonl next to the etl
//   venera-trace decode ETL JSONL     decode again
// events.jsonl has one JSON object per line, told apart by "k":
//   meta      tsc_hz (the TSC's rate, measured against QPC), events_lost, buffers_lost
//   proc      pid, name           thread  pid, tid, name      (processes and named threads seen at start and stop)
//   begin     t, pid, tid, name, v         end  t, pid, tid
//   instant   t, pid, tid, name, id, v     counter  t, pid, tid, name, v
//   etw       t, pid, tid, p (provider), id, ev (task/opcode), f (fields, formatted as strings)
// t is the TSC. venera-trace itself writes instants "venera-trace.sync" with its own reading of the TSC as v, at
// start and stop: vtrace.py checks that t matches v.
#include <windows.h>

#include <evntcons.h>
#include <evntprov.h>
#include <evntrace.h>
#include <intrin.h>
#include <tdh.h>
#include <tlhelp32.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <memory>
#include <string>
#include <tuple>
#include <vector>

// Missing from MinGW's tdh.h and evntprov.h.
extern "C" ULONG __stdcall TdhFormatProperty(PTRACE_EVENT_INFO, PEVENT_MAP_INFO, ULONG, USHORT, USHORT, USHORT, USHORT,
                                             PBYTE, PULONG, PWCHAR, PUSHORT);
struct EventIdFilter {
    BOOLEAN FilterIn;
    UCHAR Reserved;
    USHORT Count;
    USHORT Events[8];
};
constexpr ULONG kEventFilterTypeEventId = 0x80000200;

namespace {

const wchar_t kSession[] = L"venera-trace";

// {6ceeacf1-f746-4fe6-bff7-a596fede9b4e}: umd/common/trace.cpp.
constexpr GUID kVenera = {0x6ceeacf1, 0xf746, 0x4fe6, {0xbf, 0xf7, 0xa5, 0x96, 0xfe, 0xde, 0x9b, 0x4e}};
constexpr GUID kDxgKrnl = {0x802ec45a, 0x1e99, 0x4b83, {0x99, 0x20, 0x87, 0xc9, 0x82, 0x77, 0xba, 0x9d}};
constexpr GUID kDwm = {0x9e9bba3c, 0x2e38, 0x40cb, {0x99, 0xf4, 0x9e, 0x82, 0x81, 0x42, 0x51, 0x64}};

// The events of other providers that are recorded and decoded (vtrace.py reads them).
struct Provider {
    const GUID* guid;
    const char* name;
    std::vector<USHORT> events;
};
const Provider kProviders[] = {
    // VSyncDPC, MMIOFlip, Present, QueuePacket start, DmaPacket start and info (completion).
    {&kDxgKrnl, "DxgKrnl", {17, 116, 184, 178, 175, 177}},
    // Frame start and stop.
    {&kDwm, "Dwm", {10, 12}},
};

// TDH pads some names and map values with a trailing space: dropped.
std::string utf8(const wchar_t* s)
{
    const int n = WideCharToMultiByte(CP_UTF8, 0, s, -1, nullptr, 0, nullptr, nullptr);
    std::string out(n > 0 ? n - 1 : 0, '\0');
    if (n > 1)
        WideCharToMultiByte(CP_UTF8, 0, s, -1, out.data(), n, nullptr, nullptr);
    while (!out.empty() && out.back() == ' ')
        out.pop_back();
    return out;
}

std::string json_string(const std::string& s)
{
    std::string out = "\"";
    for (unsigned char c : s) {
        if (c == '"' || c == '\\') {
            out += '\\';
            out += static_cast<char>(c);
        } else if (c < 0x20) {
            char esc[8];
            std::snprintf(esc, sizeof(esc), "\\u%04x", c);
            out += esc;
        } else {
            out += static_cast<char>(c);
        }
    }
    return out + "\"";
}

// --- Session -------------------------------------------------------------------------------------------------------

struct Properties {
    EVENT_TRACE_PROPERTIES p;
    wchar_t logfile[MAX_PATH];
    wchar_t session[64];
};

std::unique_ptr<Properties> properties()
{
    auto props = std::make_unique<Properties>();
    std::memset(props.get(), 0, sizeof(Properties));
    props->p.Wnode.BufferSize = sizeof(Properties);
    props->p.LoggerNameOffset = offsetof(Properties, session);
    props->p.LogFileNameOffset = offsetof(Properties, logfile);
    return props;
}

void stop_session(Properties& props)
{
    ControlTraceW(0, kSession, &props.p, EVENT_TRACE_CONTROL_STOP);
}

void enable(TRACEHANDLE session, const GUID& guid, const std::vector<USHORT>& events)
{
    ENABLE_TRACE_PARAMETERS params{};
    params.Version = ENABLE_TRACE_PARAMETERS_VERSION_2;
    EventIdFilter filter{};
    EVENT_FILTER_DESCRIPTOR desc{};
    if (!events.empty()) {
        filter.FilterIn = TRUE;
        filter.Count = static_cast<USHORT>(events.size());
        std::memcpy(filter.Events, events.data(), events.size() * sizeof(USHORT));
        desc.Ptr = reinterpret_cast<ULONGLONG>(&filter);
        desc.Size = offsetof(EventIdFilter, Events) + filter.Count * sizeof(USHORT);
        desc.Type = kEventFilterTypeEventId;
        params.EnableFilterDesc = &desc;
        params.FilterDescCount = 1;
    }
    ULONG status = EnableTraceEx2(session, &guid, EVENT_CONTROL_CODE_ENABLE_PROVIDER, TRACE_LEVEL_VERBOSE, ~0ull, 0, 0,
                                  &params);
    if (status != ERROR_SUCCESS && !events.empty()) {
        // Without the filter: the decoder skips what it does not want.
        params.EnableFilterDesc = nullptr;
        params.FilterDescCount = 0;
        status = EnableTraceEx2(session, &guid, EVENT_CONTROL_CODE_ENABLE_PROVIDER, TRACE_LEVEL_VERBOSE, ~0ull, 0, 0,
                                &params);
    }
    if (status != ERROR_SUCCESS)
        std::fprintf(stderr, "venera-trace: enabling a provider failed: %lu\n", status);
}

// The sync instants: written through the Venera provider like the UMDs' events.
void write_sync()
{
    REGHANDLE handle = 0;
    if (EventRegister(&kVenera, nullptr, nullptr, &handle) != ERROR_SUCCESS)
        return;
    const char name[] = "venera-trace.sync";
    const uint64_t id = 0, tsc = __rdtsc();
    EVENT_DATA_DESCRIPTOR d[3];
    EventDataDescCreate(&d[0], name, sizeof(name));
    EventDataDescCreate(&d[1], &id, sizeof(id));
    EventDataDescCreate(&d[2], &tsc, sizeof(tsc));
    EVENT_DESCRIPTOR e;
    EventDescCreate(&e, 3, 0, 0, 4, 0, 0, 0);
    EventWrite(handle, &e, 3, d);
    EventUnregister(handle);
}

// Processes and named threads, as JSON lines.
void write_names(FILE* out)
{
    HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS | TH32CS_SNAPTHREAD, 0);
    if (snap == INVALID_HANDLE_VALUE)
        return;
    PROCESSENTRY32W pe{};
    pe.dwSize = sizeof(pe);
    for (BOOL ok = Process32FirstW(snap, &pe); ok; ok = Process32NextW(snap, &pe))
        std::fprintf(out, "{\"k\":\"proc\",\"pid\":%lu,\"name\":%s}\n", pe.th32ProcessID,
                     json_string(utf8(pe.szExeFile)).c_str());
    THREADENTRY32 te{};
    te.dwSize = sizeof(te);
    for (BOOL ok = Thread32First(snap, &te); ok; ok = Thread32Next(snap, &te)) {
        HANDLE t = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, FALSE, te.th32ThreadID);
        if (!t)
            continue;
        wchar_t* desc = nullptr;
        if (SUCCEEDED(GetThreadDescription(t, &desc)) && desc && desc[0])
            std::fprintf(out, "{\"k\":\"thread\",\"pid\":%lu,\"tid\":%lu,\"name\":%s}\n", te.th32OwnerProcessID,
                         te.th32ThreadID, json_string(utf8(desc)).c_str());
        LocalFree(desc);
        CloseHandle(t);
    }
    CloseHandle(snap);
}

std::wstring widen(const char* s)
{
    const int n = MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
    std::wstring out(n > 0 ? n - 1 : 0, L'\0');
    if (n > 1)
        MultiByteToWideChar(CP_UTF8, 0, s, -1, out.data(), n);
    return out;
}

int start(const char* dir_arg)
{
    const std::wstring dir = widen(dir_arg);
    CreateDirectoryW(dir.c_str(), nullptr);
    const std::wstring etl = dir + L"\\trace.etl";
    auto props = properties();
    stop_session(*props);   // a session left over from an earlier run

    props = properties();
    wcsncpy(props->logfile, etl.c_str(), MAX_PATH - 1);
    props->p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    props->p.Wnode.ClientContext = 3;   // timestamps from the CPU's cycle counter: the TSC
    props->p.LogFileMode = EVENT_TRACE_FILE_MODE_SEQUENTIAL;
    props->p.BufferSize = 1024;   // KB
    props->p.MinimumBuffers = 64;
    props->p.MaximumBuffers = 512;
    TRACEHANDLE session = 0;
    const ULONG status = StartTraceW(&session, kSession, &props->p);
    if (status != ERROR_SUCCESS) {
        std::fprintf(stderr, "venera-trace: StartTrace failed: %lu (run elevated)\n", status);
        return 1;
    }
    enable(session, kVenera, {});
    for (const Provider& p : kProviders)
        enable(session, *p.guid, p.events);
    write_sync();

    FILE* names = _wfopen((dir + L"\\names.jsonl").c_str(), L"w");
    if (names) {
        write_names(names);
        std::fclose(names);
    }
    std::printf("tracing into %s\n", utf8(etl.c_str()).c_str());
    return 0;
}

// --- Decoding ------------------------------------------------------------------------------------------------------

struct Decoder {
    FILE* out = nullptr;
    // TDH's description of each event kind, by provider, id and version.
    std::map<std::tuple<const GUID*, USHORT, UCHAR>, std::vector<BYTE>> infos;
    uint64_t count = 0;

    const Provider* provider_of(const GUID& g)
    {
        for (const Provider& p : kProviders)
            if (IsEqualGUID(g, *p.guid))
                return &p;
        return nullptr;
    }

    void venera(const EVENT_RECORD& r)
    {
        const char* data = static_cast<const char*>(r.UserData);
        const size_t size = r.UserDataLength;
        const EVENT_HEADER& h = r.EventHeader;
        const unsigned long long t = h.TimeStamp.QuadPart;
        if (h.EventDescriptor.Id == 2) {
            std::fprintf(out, "{\"k\":\"end\",\"t\":%llu,\"pid\":%lu,\"tid\":%lu}\n", t, h.ProcessId, h.ThreadId);
            return;
        }
        const size_t len = strnlen(data, size);
        const size_t ints = len < size ? (size - len - 1) / 8 : 0;
        uint64_t v[2] = {};
        std::memcpy(v, data + len + 1, (ints < 2 ? ints : 2) * 8);
        const std::string name = json_string(std::string(data, len));
        switch (h.EventDescriptor.Id) {
        case 1:
            std::fprintf(out, "{\"k\":\"begin\",\"t\":%llu,\"pid\":%lu,\"tid\":%lu,\"name\":%s,\"v\":%llu}\n", t,
                         h.ProcessId, h.ThreadId, name.c_str(), static_cast<unsigned long long>(v[0]));
            break;
        case 3:
            std::fprintf(out,
                         "{\"k\":\"instant\",\"t\":%llu,\"pid\":%lu,\"tid\":%lu,\"name\":%s,\"id\":%llu,\"v\":%llu}\n",
                         t, h.ProcessId, h.ThreadId, name.c_str(), static_cast<unsigned long long>(v[0]),
                         static_cast<unsigned long long>(v[1]));
            break;
        case 4:
            std::fprintf(out, "{\"k\":\"counter\",\"t\":%llu,\"pid\":%lu,\"tid\":%lu,\"name\":%s,\"v\":%lld}\n", t,
                         h.ProcessId, h.ThreadId, name.c_str(), static_cast<long long>(v[0]));
            break;
        }
    }

    TRACE_EVENT_INFO* info_of(const Provider& p, EVENT_RECORD& r)
    {
        auto key = std::make_tuple(p.guid, r.EventHeader.EventDescriptor.Id, r.EventHeader.EventDescriptor.Version);
        auto it = infos.find(key);
        if (it == infos.end()) {
            std::vector<BYTE> buf;
            ULONG size = 0;
            if (TdhGetEventInformation(&r, 0, nullptr, nullptr, &size) == ERROR_INSUFFICIENT_BUFFER) {
                buf.resize(size);
                if (TdhGetEventInformation(&r, 0, nullptr, reinterpret_cast<TRACE_EVENT_INFO*>(buf.data()), &size) !=
                    ERROR_SUCCESS)
                    buf.clear();
            }
            it = infos.emplace(key, std::move(buf)).first;
        }
        return it->second.empty() ? nullptr : reinterpret_cast<TRACE_EVENT_INFO*>(it->second.data());
    }

    void other(const Provider& p, EVENT_RECORD& r)
    {
        const EVENT_HEADER& h = r.EventHeader;
        bool wanted = false;
        for (USHORT id : p.events)
            wanted |= id == h.EventDescriptor.Id;
        if (!wanted)
            return;
        TRACE_EVENT_INFO* info = info_of(p, r);
        if (!info)
            return;
        auto text = [&](ULONG offset) { return offset ? utf8(reinterpret_cast<wchar_t*>(
                                                            reinterpret_cast<BYTE*>(info) + offset))
                                                      : std::string(); };
        std::string ev = text(info->TaskNameOffset);
        const std::string opcode = text(info->OpcodeNameOffset);
        if (!opcode.empty())
            ev += "/" + opcode;
        std::fprintf(out, "{\"k\":\"etw\",\"t\":%llu,\"pid\":%lu,\"tid\":%lu,\"p\":\"%s\",\"id\":%u,\"ev\":%s,\"f\":{",
                     static_cast<unsigned long long>(h.TimeStamp.QuadPart), h.ProcessId, h.ThreadId, p.name,
                     h.EventDescriptor.Id, json_string(ev).c_str());
        // The top-level scalar properties, in order, up to the first that is not one (arrays, structures, lengths
        // from other properties: none of the events read have them before the fields wanted).
        const ULONG pointer = (h.Flags & EVENT_HEADER_FLAG_32_BIT_HEADER) ? 4 : 8;
        BYTE* data = static_cast<BYTE*>(r.UserData);
        USHORT left = r.UserDataLength;
        std::vector<BYTE> map;
        wchar_t value[512];
        for (ULONG i = 0; i < info->TopLevelPropertyCount && left; i++) {
            const EVENT_PROPERTY_INFO& prop = info->EventPropertyInfoArray[i];
            if ((prop.Flags & (PropertyStruct | PropertyParamLength | PropertyParamCount)) || prop.count != 1)
                break;
            PEVENT_MAP_INFO map_info = nullptr;
            if (prop.nonStructType.MapNameOffset) {
                ULONG size = 0;
                const wchar_t* map_name =
                    reinterpret_cast<wchar_t*>(reinterpret_cast<BYTE*>(info) + prop.nonStructType.MapNameOffset);
                if (TdhGetEventMapInformation(&r, const_cast<wchar_t*>(map_name), nullptr, &size) ==
                    ERROR_INSUFFICIENT_BUFFER) {
                    map.resize(size);
                    map_info = reinterpret_cast<PEVENT_MAP_INFO>(map.data());
                    if (TdhGetEventMapInformation(&r, const_cast<wchar_t*>(map_name), map_info, &size) !=
                        ERROR_SUCCESS)
                        map_info = nullptr;
                }
            }
            ULONG size = sizeof(value);
            USHORT used = 0;
            if (TdhFormatProperty(info, map_info, pointer, prop.nonStructType.InType, prop.nonStructType.OutType,
                                  prop.length, left, data, &size, value, &used) != ERROR_SUCCESS)
                break;
            std::fprintf(out, "%s%s:%s", i ? "," : "", json_string(text(prop.NameOffset)).c_str(),
                         json_string(utf8(value)).c_str());
            data += used;
            left -= used;
        }
        std::fputs("}}\n", out);
    }

    void event(EVENT_RECORD& r)
    {
        count++;
        if (IsEqualGUID(r.EventHeader.ProviderId, kVenera))
            venera(r);
        else if (const Provider* p = provider_of(r.EventHeader.ProviderId))
            other(*p, r);
    }

    static void WINAPI callback(PEVENT_RECORD r) { static_cast<Decoder*>(r->UserContext)->event(*r); }
};

// The TSC's rate, against QPC.
double tsc_hz()
{
    LARGE_INTEGER f, q0, q1;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&q0);
    const uint64_t t0 = __rdtsc();
    Sleep(200);
    QueryPerformanceCounter(&q1);
    const uint64_t t1 = __rdtsc();
    return static_cast<double>(t1 - t0) * f.QuadPart / (q1.QuadPart - q0.QuadPart);
}

int decode(const std::wstring& etl, const std::wstring& jsonl, const std::string& meta)
{
    Decoder d;
    d.out = _wfopen(jsonl.c_str(), L"w");
    if (!d.out) {
        std::fprintf(stderr, "venera-trace: cannot write %s\n", utf8(jsonl.c_str()).c_str());
        return 1;
    }
    std::fprintf(d.out, "%s\n", meta.c_str());
    // The names taken at start, then the ones now.
    const std::wstring names = etl.substr(0, etl.find_last_of(L'\\') + 1) + L"names.jsonl";
    if (FILE* f = _wfopen(names.c_str(), L"r")) {
        char line[4096];
        while (std::fgets(line, sizeof(line), f))
            std::fputs(line, d.out);
        std::fclose(f);
    }
    write_names(d.out);

    EVENT_TRACE_LOGFILEW log{};
    log.LogFileName = const_cast<wchar_t*>(etl.c_str());
    log.ProcessTraceMode = PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
    log.EventRecordCallback = Decoder::callback;
    log.Context = &d;
    TRACEHANDLE h = OpenTraceW(&log);
    if (h == INVALID_PROCESSTRACE_HANDLE) {
        std::fprintf(stderr, "venera-trace: cannot open %s: %lu\n", utf8(etl.c_str()).c_str(), GetLastError());
        std::fclose(d.out);
        return 1;
    }
    const ULONG status = ProcessTrace(&h, 1, nullptr, nullptr);
    CloseTrace(h);
    std::fclose(d.out);
    if (status != ERROR_SUCCESS) {
        std::fprintf(stderr, "venera-trace: ProcessTrace failed: %lu\n", status);
        return 1;
    }
    std::printf("decoded %llu events into %s\n", static_cast<unsigned long long>(d.count), utf8(jsonl.c_str()).c_str());
    return 0;
}

std::string meta_line(double hz, ULONG events_lost, ULONG buffers_lost)
{
    char line[256];
    std::snprintf(line, sizeof(line), "{\"k\":\"meta\",\"tsc_hz\":%.0f,\"events_lost\":%lu,\"buffers_lost\":%lu}", hz,
                  events_lost, buffers_lost);
    return line;
}

int stop()
{
    write_sync();
    auto props = properties();
    const ULONG status = ControlTraceW(0, kSession, &props->p, EVENT_TRACE_CONTROL_STOP);
    if (status != ERROR_SUCCESS) {
        std::fprintf(stderr, "venera-trace: no trace running (%lu)\n", status);
        return 1;
    }
    if (props->p.EventsLost || props->p.RealTimeBuffersLost)
        std::fprintf(stderr, "venera-trace: %lu events lost: the trace has gaps\n", props->p.EventsLost);
    const std::wstring etl = props->logfile;
    const std::wstring jsonl = etl.substr(0, etl.find_last_of(L'\\') + 1) + L"events.jsonl";
    return decode(etl, jsonl, meta_line(tsc_hz(), props->p.EventsLost, props->p.RealTimeBuffersLost));
}

int usage()
{
    std::fputs("usage: venera-trace record SECONDS DIR | start DIR | stop | decode ETL JSONL\n", stderr);
    return 2;
}

} // namespace

int main(int argc, char** argv)
{
    if (argc < 2)
        return usage();
    const std::string cmd = argv[1];
    if (cmd == "start" && argc == 3)
        return start(argv[2]);
    if (cmd == "stop" && argc == 2)
        return stop();
    if (cmd == "record" && argc == 4) {
        const int rc = start(argv[3]);
        if (rc)
            return rc;
        Sleep(static_cast<DWORD>(std::atof(argv[2]) * 1000));
        return stop();
    }
    if (cmd == "decode" && argc == 4)
        return decode(widen(argv[2]), widen(argv[3]), meta_line(tsc_hz(), 0, 0));
    return usage();
}
