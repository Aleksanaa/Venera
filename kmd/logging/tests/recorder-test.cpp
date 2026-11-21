#include "logging.h"
#include <evntprov.h>
#include <wpprecorder.h>
#include <cassert>
#include <vector>

UCHAR TestIrql = 0;
bool TestEtwEnabled = false;
NTSTATUS TestRegistrationStatus = 0;
struct Recorded { USHORT event; UCHAR recorderLevel, payloadLevel; NTSTATUS status; };
static std::vector<Recorded> recorded;
static std::vector<UCHAR> delivered;
void EtwWrite(REGHANDLE, const EVENT_DESCRIPTOR* event, void*, ULONG, EVENT_DATA_DESCRIPTOR*) {
    if (TestEtwEnabled) delivered.push_back(event->Level);
}
void RecorderEvent(UCHAR recorderLevel, int, RECORDER_LOG, const char*,
    ULONGLONG, ULONGLONG, ULONGLONG, USHORT eventId, UCHAR version,
    UCHAR payloadLevel, ULONG, NTSTATUS status, const char*, const char*) {
    assert(version == 1);
    recorded.push_back({eventId, recorderLevel, payloadLevel, status});
}
int main() {
    VeneraLogInitialize(nullptr, nullptr);
    VeneraLogResult(0);
    VeneraLogPrintf(1, "DPC", 1360, "fatal");
    VeneraLogPrintf(2, "DPC", 1361, "error");
    VeneraLogPrintf(4, "DPC", 1362, "information");
    assert(recorded.size() == 4 && delivered.empty());
    assert(recorded[0].event == 2 && recorded[0].recorderLevel == 4 && recorded[0].payloadLevel == 4);
    assert(recorded[1].event == 3 && recorded[1].recorderLevel == 4 && recorded[1].payloadLevel == 4);
    assert(recorded[2].event == 1 && recorded[2].recorderLevel == 1 && recorded[2].payloadLevel == 1);
    assert(recorded[3].event == 1 && recorded[3].recorderLevel == 2 && recorded[3].payloadLevel == 2);
    TestEtwEnabled = true;
    VeneraLogPrintf(4, "DPC", 1, "information");
    assert(recorded.size() == 4 && delivered == std::vector<UCHAR>{4});
    TestIrql = 3;
    VeneraLogPrintf(1, "DPC", 1, "fatal");
    assert(recorded.size() == 4 && delivered == (std::vector<UCHAR>{4, 1}));
    TestIrql = 2;
    VeneraLogPrintf(1, "DPC", 1, "fatal at dispatch");
    assert(recorded.size() == 5 && recorded.back().recorderLevel == 1);
    VeneraLogCleanup(nullptr);
    assert(recorded.size() == 6 && recorded.back().event == 4);
    assert(recorded.back().recorderLevel == 4 && recorded.back().payloadLevel == 4);
    TestEtwEnabled = false;
    TestRegistrationStatus = -1;
    VeneraLogInitialize(nullptr, nullptr);
    VeneraLogPrintf(1, "DPC", 1, "provider registration failed");
    assert(recorded.size() == 8 && recorded[6].status == -1 && recorded[6].recorderLevel == 2);
    assert(recorded[7].recorderLevel == 1 && recorded[7].payloadLevel == 1);
    VeneraLogCleanup(nullptr);
}
