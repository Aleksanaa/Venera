#pragma once
#include <cstdint>
#include <cstring>
#include <cstdarg>
#include <cstdio>
using UCHAR = uint8_t;
using USHORT = uint16_t;
using ULONG = uint32_t;
using ULONGLONG = uint64_t;
using LONG64 = int64_t;
using NTSTATUS = int32_t;
using BOOLEAN = bool;
using PDRIVER_OBJECT = void*;
using PCUNICODE_STRING = const void*;
using REGHANDLE = uint64_t;
struct LARGE_INTEGER { int64_t QuadPart; };
struct GUID { uint32_t a; uint16_t b, c; uint8_t d[8]; };
constexpr bool FALSE = false;
constexpr NTSTATUS STATUS_SUCCESS = 0;
constexpr UCHAR DISPATCH_LEVEL = 2;
#define NT_SUCCESS(status) ((status) >= 0)
#define RTL_NUMBER_OF(array) (sizeof(array) / sizeof(array[0]))
extern UCHAR TestIrql;
inline UCHAR KeGetCurrentIrql() { return TestIrql; }
inline void KeQuerySystemTimePrecise(LARGE_INTEGER* time) { time->QuadPart = 134393633912317080; }
inline ULONGLONG KeQueryInterruptTimePrecise(ULONGLONG* qpc) { *qpc = 1; return 42; }
inline LONG64 InterlockedIncrement64(volatile LONG64* value) { return ++*value; }
