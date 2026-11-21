// SPDX-License-Identifier: MIT
#pragma once
#include <ntddk.h>

#ifdef __cplusplus
extern "C" {
#endif

void VeneraLogInitialize(PDRIVER_OBJECT driver, PCUNICODE_STRING path);
void VeneraLogCleanup(PDRIVER_OBJECT driver);
void VeneraLogResult(NTSTATUS status);
void VeneraLogPrintf(UCHAR level, const char *function, ULONG line, const char *format, ...);
#ifdef __cplusplus
}
#endif
