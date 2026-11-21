// SPDX-License-Identifier: MIT
#pragma once
#include <cstdint>
using DWORD = unsigned long;
using ULONG = unsigned long;
using USHORT = uint16_t;
using UCHAR = uint8_t;
using ULONGLONG = uint64_t;
using HANDLE = void*;
using BOOL = int;
struct GUID { uint32_t a; uint16_t b, c; uint8_t d[8]; };
struct FILETIME { DWORD dwLowDateTime, dwHighDateTime; };
struct SYSTEMTIME { USHORT wYear, wMonth, wDayOfWeek, wDay, wHour, wMinute, wSecond, wMilliseconds; };
#define INVALID_HANDLE_VALUE reinterpret_cast<HANDLE>(-1)
#define HKEY_LOCAL_MACHINE nullptr
constexpr DWORD ERROR_SUCCESS = 0, RRF_RT_REG_SZ = 1, RRF_RT_REG_DWORD = 2;
constexpr DWORD FILE_APPEND_DATA = 4, FILE_SHARE_READ = 1, FILE_SHARE_WRITE = 2, FILE_SHARE_DELETE = 4;
constexpr DWORD OPEN_ALWAYS = 4, FILE_ATTRIBUTE_NORMAL = 128, FILE_FLAG_WRITE_THROUGH = 0x80000000;
constexpr unsigned MAX_PATH = 260;
DWORD GetEnvironmentVariableA(const char*, char*, DWORD);
DWORD RegGetValueA(HANDLE, const char*, const char*, DWORD, DWORD*, void*, DWORD*);
BOOL SetEnvironmentVariableA(const char*, const char*);
DWORD GetModuleFileNameA(HANDLE, char*, DWORD);
HANDLE GetCurrentProcess();
BOOL GetProcessTimes(HANDLE, FILETIME*, FILETIME*, FILETIME*, FILETIME*);
HANDLE CreateFileA(const char*, DWORD, DWORD, void*, DWORD, DWORD, HANDLE);
BOOL WriteFile(HANDLE, const void*, DWORD, DWORD*, void*);
void GetSystemTimePreciseAsFileTime(FILETIME*);
void GetLocalTime(SYSTEMTIME*);
DWORD GetCurrentProcessId();
DWORD GetCurrentThreadId();
void OutputDebugStringA(const char*);
