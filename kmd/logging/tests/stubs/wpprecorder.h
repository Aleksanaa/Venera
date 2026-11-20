#pragma once
#include "ntddk.h"
using RECORDER_LOG = void*;
struct RECORDER_LOG_CREATE_PARAMS { ULONG TotalBufferSize, ErrorPartitionSize; };
inline void RECORDER_LOG_CREATE_PARAMS_INIT(RECORDER_LOG_CREATE_PARAMS* params, const char*) { *params = {}; }
inline NTSTATUS WppRecorderLogCreate(RECORDER_LOG_CREATE_PARAMS*, RECORDER_LOG* log) { *log = reinterpret_cast<void*>(1); return 0; }
inline RECORDER_LOG WppRecorderLogGetDefault() { return reinterpret_cast<void*>(1); }
inline void WppRecorderLogDelete(RECORDER_LOG) {}
