// SPDX-License-Identifier: MIT
#include "logging.h"
#include <evntprov.h>
#include <ntstrsafe.h>
#include <wpprecorder.h>

#define WPP_CONTROL_GUIDS WPP_DEFINE_CONTROL_GUID(VeneraRecorder, (88FF4AF9,B7B4,40FF,94AD,271B4777ED97), WPP_DEFINE_BIT(TRACE_ALL))
#define WPP_LEVEL_FLAGS_LOGGER(level, flags) WPP_LEVEL_LOGGER(flags)
#define WPP_LEVEL_FLAGS_ENABLED(level, flags) (WPP_LEVEL_ENABLED(flags) && WPP_CONTROL(WPP_BIT_ ## flags).Level >= level)
// begin_wpp config
// FUNC RecorderEvent(LEVEL, FLAGS, IFRLOG, MSG, ...);
// end_wpp
#include "generated/recorder.tmh"

// Venera kernel event schema, version 1.
static const GUID Provider = {0x6c5a2378, 0x22b9, 0x4c10, {0xa9, 0x1f, 0x69, 0xe3, 0x4c, 0x28, 0x0b, 0x81}};
static REGHANDLE ProviderHandle;
static ULONGLONG ProducerInstance;
static volatile LONG64 Sequence;
static RECORDER_LOG StartupLog, DefaultLog;
static BOOLEAN OwnRecorderLog;
static BOOLEAN IsFailure(UCHAR level)
{
    return level == 1 || level == 2;
}

static void Emit(USHORT eventId, UCHAR level, ULONG line, NTSTATUS status,
                 const char *function, const char *message)
{
    const UCHAR version = 1;
    const ULONGLONG producerInstance = ProducerInstance;
    const ULONGLONG sequence = (ULONGLONG)InterlockedIncrement64(&Sequence);
    LARGE_INTEGER now;
    KeQuerySystemTimePrecise(&now);
    const ULONGLONG sourceTime = (ULONGLONG)now.QuadPart;
    EVENT_DESCRIPTOR descriptor = {eventId, version, 0, level, 0, 0, 1};
    EVENT_DATA_DESCRIPTOR data[10];
    EventDataDescCreate(&data[0], &producerInstance, sizeof(producerInstance));
    EventDataDescCreate(&data[1], &sequence, sizeof(sequence));
    EventDataDescCreate(&data[2], &sourceTime, sizeof(sourceTime));
    EventDataDescCreate(&data[3], &eventId, sizeof(eventId));
    EventDataDescCreate(&data[4], &version, sizeof(version));
    EventDataDescCreate(&data[5], &level, sizeof(level));
    EventDataDescCreate(&data[6], &line, sizeof(line));
    EventDataDescCreate(&data[7], &status, sizeof(status));
    EventDataDescCreate(&data[8], function, (ULONG)strlen(function) + 1);
    EventDataDescCreate(&data[9], message, (ULONG)strlen(message) + 1);
    if (ProviderHandle) {
        EtwWrite(ProviderHandle, &descriptor, NULL, RTL_NUMBER_OF(data), data);
    }
    const RECORDER_LOG RecorderLog = eventId == 1 ? DefaultLog : StartupLog;
    if (RecorderLog && (eventId != 1 || IsFailure(level)) && KeGetCurrentIrql() <= DISPATCH_LEVEL) {
        RecorderEvent(level, TRACE_ALL, RecorderLog, "instance=%I64u sequence=%I64u time=%I64u event=%hu version=%!UCHAR! level=%!UCHAR! line=%lu status=%ld function=%s message=%s",
                      producerInstance, sequence, sourceTime, eventId, version, level, line, status, function, message);
    }
}

void VeneraLogInitialize(PDRIVER_OBJECT driver, PCUNICODE_STRING path)
{
    ULONGLONG qpc;
    ProducerInstance = KeQueryInterruptTimePrecise(&qpc);
    WPP_INIT_TRACING(driver, path);
    RECORDER_LOG_CREATE_PARAMS params;
    RECORDER_LOG_CREATE_PARAMS_INIT(&params, "Venera kernel");
    params.TotalBufferSize = 64 * 1024;
    params.ErrorPartitionSize = 16 * 1024;
    OwnRecorderLog = NT_SUCCESS(WppRecorderLogCreate(&params, &StartupLog));
    DefaultLog = WppRecorderLogGetDefault();
    if (!OwnRecorderLog) {
        StartupLog = DefaultLog;
    }
    NTSTATUS status = EtwRegister(&Provider, NULL, NULL, &ProviderHandle);
    Emit(2, NT_SUCCESS(status) ? 4 : 2, 0, status, "DriverEntry", "Venera kernel initialization");
}

void VeneraLogResult(NTSTATUS status)
{
    Emit(3, NT_SUCCESS(status) ? 4 : 2, 0, status, "DxgkInitialize", "Display driver registration");
}

void VeneraLogCleanup(PDRIVER_OBJECT driver)
{
    Emit(4, 4, 0, STATUS_SUCCESS, "DriverUnload", "Venera kernel unload");
    if (OwnRecorderLog) {
        WppRecorderLogDelete(StartupLog);
        OwnRecorderLog = FALSE;
    }
    StartupLog = NULL;
    DefaultLog = NULL;
    if (ProviderHandle) {
        EtwUnregister(ProviderHandle);
        ProviderHandle = 0;
    }
    WPP_CLEANUP(driver);
}

void VeneraLogPrintf(UCHAR level, const char *function, ULONG line, const char *format, ...)
{
    EVENT_DESCRIPTOR descriptor = {1, 1, 0, level, 0, 0, 1};
    if (!IsFailure(level) && (!ProviderHandle || !EtwEventEnabled(ProviderHandle, &descriptor))) {
        return;
    }
    char message[1024];
    va_list args;
    va_start(args, format);
    RtlStringCbVPrintfA(message, sizeof(message), format, args);
    va_end(args);
    message[sizeof(message) - 1] = '\0';
    Emit(1, level, line, STATUS_SUCCESS, function, message);
}
