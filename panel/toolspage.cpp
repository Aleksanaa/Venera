// SPDX-License-Identifier: MIT
// Tools. The frame rate overlay runs as venera-panel /fps (fpsoverlay.cpp), a process of its own with administrator
// rights, which outlives the panel; the toggle follows its window, also when Ctrl+Alt+F closed it. Troubleshooting:
// opening an issue on GitHub, and restarting the Desktop Window Manager (through venera-panel /restart-dwm and UAC when
// this panel runs without administrator rights).
#include "panel.h"

#include <shellapi.h>

namespace panel {

using namespace theme;

namespace {

constexpr UINT_PTR kWatchTimer = kPageTimer + 1;

constexpr wchar_t kNewIssue[] = L"https://github.com/Aleksanaa/venera/issues/new";

class Tools : public Page {
public:
    explicit Tools(HWND window) : window_(window) { SetTimer(window_, kWatchTimer, 500, nullptr); }
    ~Tools() override
    {
        if (starting_)
            CloseHandle(starting_);
    }
    const wchar_t* title() const override { return L"Tools"; }
    const wchar_t* icon() const override { return L""; }
    void draw(Ui& ui, float x, float w, float& y) override;
    void click(int id) override;
    bool message(UINT msg, WPARAM wp, LPARAM lp) override;

private:
    enum : int { kFpsId = kPageId, kIssueId, kDwmId };

    bool overlayOn() const { return starting_ || findFpsOverlay(); }
    void startOverlay();

    HWND window_;
    HANDLE starting_ = nullptr;  // the overlay's process until its window shows (or it exits, when it could not start)
    bool shown_ = overlayOn();   // what the toggle showed last
};

void Tools::draw(Ui& ui, float x, float w, float& y)
{
    shown_ = overlayOn();
    float cy = ui.settingsCard(x, y, w, L"", L"Frame rate overlay",
                               L"Frame rate, hitches, and each program's FPS and GPU memory");
    ui.toggle(x + w - 16, cy, kFpsId, shown_);
    ui.sectionHeader(L"Troubleshooting", x, y);
    cy = ui.settingsCard(x, y, w, L"", L"Report an issue",
                         L"On GitHub. Paste the Overview's report into it.");
    ui.buttonRight(L"Open", x + w - 16, cy, kIssueId, false);
    cy = ui.settingsCard(x, y, w, L"", L"Restart the Desktop Window Manager",
                         L"Reloads the drivers and their settings. The screen flickers.");
    ui.buttonRight(L"Restart", x + w - 16, cy, kDwmId, false);
}

void Tools::click(int id)
{
    if (id == kIssueId) {
        ShellExecuteW(window_, nullptr, kNewIssue, nullptr, nullptr, SW_SHOWNORMAL);
    } else if (id == kDwmId) {
        if (elevated())
            restartDwm();
        else if (HANDLE p = startSelf(window_, L"/restart-dwm", SW_SHOWNOACTIVATE))
            CloseHandle(p);
    } else if (id == kFpsId) {
        if (HWND overlay = findFpsOverlay())
            PostMessageW(overlay, WM_CLOSE, 0, 0);
        else if (!starting_)
            startOverlay();
    }
}

// It reports its own errors.
void Tools::startOverlay() { starting_ = startSelf(window_, L"/fps", SW_SHOWNOACTIVATE); }

// The overlay's window comes and goes on its own: a repaint when the toggle would change.
bool Tools::message(UINT msg, WPARAM wp, LPARAM)
{
    if (msg != WM_TIMER || wp != kWatchTimer)
        return false;
    if (starting_ && (findFpsOverlay() || WaitForSingleObject(starting_, 0) == WAIT_OBJECT_0)) {
        CloseHandle(starting_);
        starting_ = nullptr;
    }
    return overlayOn() != shown_;
}

}  // namespace

std::unique_ptr<Page> makeTools(HWND window) { return std::make_unique<Tools>(window); }

}  // namespace panel
