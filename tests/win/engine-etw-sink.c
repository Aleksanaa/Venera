/* SPDX-License-Identifier: MIT
 * Test transport for the MinGW source-runtime probes. Store EventWrite's
 * concatenated payload exactly as it would reach EVENT_RECORD.UserData.
 */
#include <windows.h>
#include <evntprov.h>
#include <stdio.h>
#include <stdlib.h>

static BOOLEAN WINAPI enabled(REGHANDLE handle, PCEVENT_DESCRIPTOR event)
{
    (void)handle; (void)event;
    return getenv("TEST_ETW") && strcmp(getenv("TEST_ETW"), "off");
}
static ULONG WINAPI write_event(REGHANDLE handle, PCEVENT_DESCRIPTOR event, ULONG count,
                                PEVENT_DATA_DESCRIPTOR data)
{
    (void)handle;
    if (!strcmp(getenv("TEST_ETW"), "fail")) return ERROR_NOT_ENOUGH_MEMORY;
    FILE *out = fopen("events.bin", "ab");
    if (!out) abort();
    ULONG bytes = 0;
    for (ULONG i = 0; i < count; i++) bytes += data[i].Size;
    fwrite(event, sizeof(*event), 1, out);
    fwrite(&bytes, sizeof(bytes), 1, out);
    for (ULONG i = 0; i < count; i++) fwrite((void *)(uintptr_t)data[i].Ptr, 1, data[i].Size, out);
    if (fclose(out)) abort();
    return ERROR_SUCCESS;
}
BOOLEAN (WINAPI *__imp_EventEnabled)(REGHANDLE,PCEVENT_DESCRIPTOR) = enabled;
ULONG (WINAPI *__imp_EventWrite)(REGHANDLE,PCEVENT_DESCRIPTOR,ULONG,PEVENT_DATA_DESCRIPTOR) = write_event;
