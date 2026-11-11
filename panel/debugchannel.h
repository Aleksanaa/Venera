// SPDX-License-Identifier: MIT
#pragma once
#include "panel.h"
#include "debugjson.h"
#include <atomic>

namespace panel {
struct DebugStatus {
    bool valid = false, forwarding = true;
    std::wstring revision, expires, driver, controller, port, host, filter, error;
    debugjson::Value effectiveFilter;
};
DebugStatus queryDebug(const std::vector<std::wstring>& args = {L"status", L"--json"});
std::wstring debugQuote(const std::wstring& value);
std::string debugJsonString(const std::wstring& value);
class GuestLogs {
public:
    ~GuestLogs() { stop(); }
    void start(std::function<void(LogLine&&)> sink);
    void stop();
    bool running() const { return running_; }
    std::wstring error() const { std::lock_guard<std::mutex> lock(mutex_); return error_; }
private:
    std::thread thread_;
    HANDLE cancel_ = nullptr;
    std::atomic<bool> running_{false};
    mutable std::mutex mutex_;
    std::wstring error_;
};
std::unique_ptr<Page> makeDebug(HWND window);
}
