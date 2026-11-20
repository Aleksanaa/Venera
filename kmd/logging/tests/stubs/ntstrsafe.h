#pragma once
#include "ntddk.h"
inline void RtlStringCbVPrintfA(char* output, size_t bytes, const char* format, va_list args) { vsnprintf(output, bytes, format, args); }
