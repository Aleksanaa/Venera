// SPDX-License-Identifier: MIT
// The log, for what happened; trace.h, for when and for how long.
//
// Log what explains behaviour: failures, fallbacks taken, capability answers, which path a process took (the first few
// presents), anomalies someone has to read (a wait stalled for seconds). Users often have it on before a problem
// shows, so it is what we get without asking for a trace. A line costs formatting, a lock shared by all threads and a
// file write (tens of us): log only what happens rarely, never per frame, batch or wait (VENERA_VERBOSE lines are
// correctness answers, high-volume only while asked for).
//
// Trace what measures it: per-frame, per-batch and per-wait timing, where the timestamp is the information. Traces cost
// a flag test unless a session records, and are read on a timeline, never as text.
//
// An event goes to one of them. An error stays a log line even where its time also shows on the timeline (the wait
// it happened in is a span already).
#pragma once

#include <initializer_list>

namespace venera::umd {

enum class LogLevel { error, warn, info, debug };

// Lines go to OutputDebugString, and also to the file named by VENERA_LOG, from the environment or else from
// HKLM\SOFTWARE\Venera (for processes that do not inherit the environment, such as DWM and browsers' GPU
// processes). Each line names `component`, the UMD writing it.
void log_init(const char* component);
// Copies each named setting from HKLM\SOFTWARE\Venera into the process environment unless the environment has it:
// the engines (vkd3d-proton, DXVK) and the Venus ICD read their options with GetEnvironmentVariable.
void forward_settings(std::initializer_list<const char*> names);
// For an engine that prints to stderr, which GUI programs do not have: unless the environment names its file, sets
// the variable `name` to <program>_<suffix>.log beside the log file (VENERA_LOG), when there is one.
void log_file_beside(const char* name, const char* suffix);
// The setting `name` from the environment or else the registry (as above); false if neither has it. A REG_DWORD
// value reads as its decimal digits.
bool setting(const char* name, char* out, unsigned long size);
// VENERA_VERBOSE=1 (likewise): also log every answer of high-volume queries (format support, MSAA levels).
bool verbose();
// Logs every thread's stack in the process (module+offset frames, thread names), for hangs: see stacks.cpp.
void log_thread_stacks(const char* why);
// Logs first-chance C++ throws and access violations with the raising thread's stack (stacks.cpp); log_init
// installs it when a log file is configured.
void log_exceptions();
void logf(LogLevel level, const char* fmt, ...)
#if defined(__GNUC__)
    __attribute__((format(printf, 2, 3)))
#endif
    ;

} // namespace venera::umd

#define VENERA_TRACE(...) ::venera::umd::logf(::venera::umd::LogLevel::debug, __VA_ARGS__)
#define VENERA_VERBOSE(...) \
    (::venera::umd::verbose() ? ::venera::umd::logf(::venera::umd::LogLevel::debug, __VA_ARGS__) : void())
#define VENERA_WARN(...) ::venera::umd::logf(::venera::umd::LogLevel::warn, __VA_ARGS__)
