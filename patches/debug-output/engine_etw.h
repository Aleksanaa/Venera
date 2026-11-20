/* SPDX-License-Identifier: MIT */
#ifndef VENERA_ENGINE_ETW_H
#define VENERA_ENGINE_ETW_H
#include <windows.h>
#include <evntprov.h>
#include <stdlib.h>
#include <string.h>

enum { VENERA_DXVK = 7, VENERA_VKD3D, VENERA_MESA_OS, VENERA_MESA_LOG };
static INIT_ONCE venera_once = INIT_ONCE_STATIC_INIT;
static REGHANDLE venera_provider;
static LUID venera_instance;
static FILETIME venera_created;
static DECLSPEC_ALIGN(8) volatile LONG64 venera_sequence;

static void venera_unregister(void)
{
    EventUnregister(venera_provider);
    venera_provider = 0;
}

static BOOL CALLBACK venera_register(PINIT_ONCE once, PVOID arg, PVOID *context)
{
    static const GUID guid = {0x6c5a2378, 0x22b9, 0x4c10, {0xa9,0x1f,0x69,0xe3,0x4c,0x28,0x0b,0x81}};
    FILETIME exit_time, kernel, user;
    (void)once; (void)arg; (void)context;
    if (!AllocateLocallyUniqueId(&venera_instance) ||
        !GetProcessTimes(GetCurrentProcess(), &venera_created, &exit_time, &kernel, &user) ||
        EventRegister(&guid, NULL, NULL, &venera_provider) != ERROR_SUCCESS)
        return FALSE;
    /* Keep engine instance IDs separate from kernel interrupt-time IDs. */
    venera_instance.HighPart |= 0x80000000u;
    if (atexit(venera_unregister)) {
        venera_unregister();
        return FALSE;
    }
    return TRUE;
}

static void venera_engine_text(USHORT id, const char *level, const char *channel,
                               const char *msg, size_t length)
{
    const EVENT_DESCRIPTOR event = {id, 1, 0, 4, 0, 0, 1};
    const size_t chunk = 60 * 1024;
    FILETIME time;
    LONG64 seq;
    ULONG part, parts;
    EVENT_DATA_DESCRIPTOR data[9];
    if (length > 16 * 1024 * 1024 || strlen(channel) > 1024 ||
        !InitOnceExecuteOnce(&venera_once, venera_register, NULL, NULL) ||
        !EventEnabled(venera_provider, &event))
        return;
    GetSystemTimePreciseAsFileTime(&time);
    seq = InterlockedIncrement64(&venera_sequence);
    parts = length ? (ULONG)(1 + (length - 1) / chunk) : 1;
    EventDataDescCreate(data, &time, 8);
    EventDataDescCreate(data+1, &venera_created, 8);
    EventDataDescCreate(data+2, &venera_instance, 8);
    EventDataDescCreate(data+3, &seq, 8);
    EventDataDescCreate(data+4, &part, 4);
    EventDataDescCreate(data+5, &parts, 4);
    EventDataDescCreate(data+6, level, (ULONG)strlen(level)+1);
    EventDataDescCreate(data+7, channel, (ULONG)strlen(channel)+1);
    for (part = 0; part < parts; ++part) {
        ULONG n = (ULONG)(length > chunk ? chunk : length);
        EventDataDescCreate(data+8, msg, n);
        if (EventWrite(venera_provider, &event, 9, data) != ERROR_SUCCESS)
            break;
        msg += n;
        length -= n;
    }
}
#endif
