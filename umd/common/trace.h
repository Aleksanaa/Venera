// SPDX-License-Identifier: MIT
// Timeline events for performance traces: the Venera ETW provider, which tests/win/venera-trace.exe records and
// tests/perf/vtrace.py turns into a Perfetto timeline next to the host's GPU and thread activity. Writing costs a
// flag test while no trace session listens. Trace or log: log.h says which.
//
// The events, by id (level 4, no keywords; all strings NUL-terminated ANSI, integers little-endian):
//   1 begin    name, u64 value   a span opens on this thread
//   2 end      -                 the thread's innermost span closes
//   3 instant  name, u64 id, u64 value
//                                a point in a pipeline: vtrace.py joins instants of one id and value across threads
//                                (a D3D12 batch through the queue bridge: id = queue, value = batch)
//   4 counter  name, i64 value
// Our Venus ICD writes begin, end and counter for Mesa's trace scopes (icd/patches/0011), through the same provider.
#pragma once

#include <atomic>
#include <cstdint>

namespace venera::trace {

namespace detail {
extern std::atomic<bool> g_enabled;
void begin(const char* name, uint64_t value);
void end();
void instant(const char* name, uint64_t id, uint64_t value);
void counter(const char* name, int64_t value);
} // namespace detail

inline bool enabled() { return detail::g_enabled.load(std::memory_order_relaxed); }

inline void instant(const char* name, uint64_t id, uint64_t value)
{
    if (enabled())
        detail::instant(name, id, value);
}

inline void counter(const char* name, int64_t value)
{
    if (enabled())
        detail::counter(name, value);
}

inline uint64_t id_of(const void* object) { return reinterpret_cast<uintptr_t>(object); }

// A span over the enclosing block. It closes whatever it opened even if the session stops in between.
class Scope {
public:
    explicit Scope(const char* name, uint64_t value = 0) : open_(enabled())
    {
        if (open_)
            detail::begin(name, value);
    }
    ~Scope()
    {
        if (open_)
            detail::end();
    }
    Scope(const Scope&) = delete;
    Scope& operator=(const Scope&) = delete;

private:
    bool open_;
};

} // namespace venera::trace
