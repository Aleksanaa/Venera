// SPDX-License-Identifier: MIT
#pragma once
#include "windows.h"
#include <cstddef>
using REGHANDLE = uint64_t;
struct EVENT_DESCRIPTOR {
    USHORT Id;
    UCHAR Version, Channel, Level, Opcode;
    USHORT Task;
    ULONGLONG Keyword;
};
struct EVENT_DATA_DESCRIPTOR { const void* data; ULONG size; };
inline void EventDataDescCreate(EVENT_DATA_DESCRIPTOR* descriptor, const void* data, ULONG size)
{
    *descriptor = {data, size};
}
ULONG EventRegister(const GUID*, void*, void*, REGHANDLE*);
ULONG EventUnregister(REGHANDLE);
BOOL EventEnabled(REGHANDLE, const EVENT_DESCRIPTOR*);
ULONG EventWrite(REGHANDLE, const EVENT_DESCRIPTOR*, ULONG, EVENT_DATA_DESCRIPTOR*);
