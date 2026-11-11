// SPDX-License-Identifier: MIT
#include "trace.h"

#include <windows.h>

#include <evntprov.h>
#include <evntrace.h>

#include <cstring>

namespace venera::trace {

namespace detail {
std::atomic<bool> g_enabled{false};
}

namespace {

// Venera: {6ceeacf1-f746-4fe6-bff7-a596fede9b4e} (tests/win/venera-trace.cpp, icd/patches/0011).
constexpr GUID kProvider = {0x6ceeacf1, 0xf746, 0x4fe6, {0xbf, 0xf7, 0xa5, 0x96, 0xfe, 0xde, 0x9b, 0x4e}};

enum : USHORT { kBegin = 1, kEnd = 2, kInstant = 3, kCounter = 4 };

void NTAPI on_enable(LPCGUID, ULONG is_enabled, UCHAR, ULONGLONG, ULONGLONG, PEVENT_FILTER_DESCRIPTOR, PVOID)
{
    // 2 asks for a state capture, which we have none of: it leaves the provider enabled.
    if (is_enabled != EVENT_CONTROL_CODE_CAPTURE_STATE)
        detail::g_enabled.store(is_enabled != EVENT_CONTROL_CODE_DISABLE_PROVIDER, std::memory_order_relaxed);
}

// Registered for the module's lifetime: the provider must be gone before the module unloads, since ETW calls
// on_enable.
struct Registration {
    REGHANDLE handle = 0;
    Registration() { EventRegister(&kProvider, on_enable, nullptr, &handle); }
    ~Registration()
    {
        detail::g_enabled.store(false, std::memory_order_relaxed);
        EventUnregister(handle);
    }
} g_registration;

void write(USHORT id, ULONG count, EVENT_DATA_DESCRIPTOR* data)
{
    EVENT_DESCRIPTOR d;
    EventDescCreate(&d, id, 0, 0, 4 /* information */, 0, 0, 0);
    EventWrite(g_registration.handle, &d, count, data);
}

void describe(EVENT_DATA_DESCRIPTOR& d, const char* name)
{
    EventDataDescCreate(&d, name, static_cast<ULONG>(std::strlen(name) + 1));
}

} // namespace

void detail::begin(const char* name, uint64_t value)
{
    EVENT_DATA_DESCRIPTOR d[2];
    describe(d[0], name);
    EventDataDescCreate(&d[1], &value, sizeof(value));
    write(kBegin, 2, d);
}

void detail::end() { write(kEnd, 0, nullptr); }

void detail::instant(const char* name, uint64_t id, uint64_t value)
{
    EVENT_DATA_DESCRIPTOR d[3];
    describe(d[0], name);
    EventDataDescCreate(&d[1], &id, sizeof(id));
    EventDataDescCreate(&d[2], &value, sizeof(value));
    write(kInstant, 3, d);
}

void detail::counter(const char* name, int64_t value)
{
    EVENT_DATA_DESCRIPTOR d[2];
    describe(d[0], name);
    EventDataDescCreate(&d[1], &value, sizeof(value));
    write(kCounter, 2, d);
}

} // namespace venera::trace
