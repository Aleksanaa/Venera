#pragma once
#include "ntddk.h"
struct EVENT_DESCRIPTOR { USHORT Id; UCHAR Version, Channel, Level, Opcode; USHORT Task; ULONGLONG Keyword; };
struct EVENT_DATA_DESCRIPTOR { const void* Ptr; ULONG Size; };
extern bool TestEtwEnabled;
extern NTSTATUS TestRegistrationStatus;
inline void EventDataDescCreate(EVENT_DATA_DESCRIPTOR* data, const void* ptr, ULONG size) { *data = {ptr, size}; }
inline NTSTATUS EtwRegister(const GUID*, void*, void*, REGHANDLE* handle) { *handle = NT_SUCCESS(TestRegistrationStatus) ? 1 : 0; return TestRegistrationStatus; }
inline bool EtwEventEnabled(REGHANDLE, const EVENT_DESCRIPTOR*) { return TestEtwEnabled; }
inline void EtwUnregister(REGHANDLE) {}
void EtwWrite(REGHANDLE, const EVENT_DESCRIPTOR*, void*, ULONG, EVENT_DATA_DESCRIPTOR*);
