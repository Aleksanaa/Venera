// SPDX-License-Identifier: MIT
// Live debug-service subscription and local display filters.
#include "panel.h"
#include "debugchannel.h"

#include <commdlg.h>
#include <shlobj.h>

#include <deque>

namespace panel {

using namespace theme;

namespace {

constexpr UINT kLines = kPageMessage + 4;
constexpr float kRowH = 22;
constexpr size_t kMaxLines = 20000;
constexpr int kLineRun = 1;
enum : int { kCaptureId = kPageId, kClearId, kExportId, kElevateId, kFilterId, kLineText = kPageId + 100 };

// The SelectorBar narrows step by step: everything, the drivers' lines, their warnings and errors, their errors.
enum Filter { kAll, kDrivers, kWarnings, kErrors };
const wchar_t* const kFilters[] = {L"All", L"Drivers", L"Warnings", L"Errors"};

std::wstring clock(const SYSTEMTIME& t, bool ms)
{
    wchar_t buf[16];
    if (ms)
        swprintf(buf, 16, L"%02u:%02u:%02u.%03u", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds);
    else
        swprintf(buf, 16, L"%02u:%02u:%02u", t.wHour, t.wMinute, t.wSecond);
    return buf;
}

struct Line : LogLine {
    unsigned seq;
};

class Logs : public Page {
public:
    explicit Logs(HWND window) : window_(window) { SetTimer(window_, kPageTimer + 31, 500, nullptr); }
    ~Logs() override { KillTimer(window_, kPageTimer + 31); capture_.stop(); }
    const wchar_t* title() const override { return L"Logs"; }
    const wchar_t* icon() const override { return L""; }
    void draw(Ui& ui, float x, float w, float& y) override;
    void click(int id) override;
    bool message(UINT msg, WPARAM wp, LPARAM lp) override;
    bool fills() const override { return true; }
    bool followsEnd() const override { return true; }

private:
    void start();
    bool shown(const Line& l) const
    {
        return filter_ == kAll || (!l.component.empty() && l.level >= filter_ - kDrivers);
    }
    void exportLog();

    HWND window_;
    GuestLogs capture_;
    SYSTEMTIME since_ = {};
    int filter_ = kAll;
    std::deque<Line> lines_;
    unsigned seq_ = 0;
    std::mutex mutex_;
    std::vector<LogLine> pending_;  // from the capture's threads
};

void Logs::start()
{
    GetLocalTime(&since_);
    capture_.start([this](LogLine&& l) {
        std::lock_guard<std::mutex> lock(mutex_);
        if (pending_.empty())
            PostMessageW(window_, kLines, 0, 0);
        if (pending_.size() < kMaxLines)
            pending_.push_back(std::move(l));
    });
}

bool Logs::message(UINT msg, WPARAM wp, LPARAM)
{
    if (msg == WM_TIMER && wp == kPageTimer + 31)
        return true;
    if (msg != kLines)
        return false;
    std::vector<LogLine> got;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        got.swap(pending_);
    }
    for (LogLine& l : got)
        lines_.push_back({std::move(l), seq_++});
    while (lines_.size() > kMaxLines)
        lines_.pop_front();
    return true;
}

void Logs::draw(Ui& ui, float x, float w, float& y)
{
    if (capture_.running()) {
        std::wstring desc = L"Since " + clock(since_, false);
        float cy = ui.settingsCard(x, y, w, L"", L"Following debug output", desc, kCritical);
        ui.buttonRight(L"Stop", x + w - 16, cy, kCaptureId, false);
    } else {
        float cy = ui.settingsCard(x, y, w, L"", L"Follow debug output",
                                   L"Live messages from the debug service");
        ui.buttonRight(L"Start", x + w - 16, cy, kCaptureId, true);
    }

    std::wstring error = capture_.error();
    if (!error.empty())
        ui.infoBar(x, w, y, Ui::Severity::kError, L"", error);
    y += 16;

    // SelectorBar for which lines, actions on the right
    float fx = x;
    for (int i = 0; i < 4; i++) {
        float tw = ui.measure(kFilters[i], Ui::kBody) + 24;
        D2D1_RECT_F r = {fx, y, fx + tw, y + 40};
        bool sel = filter_ == i, h = ui.pointer(kFilterId + i).over;
        if (h && !sel)
            ui.fill({r.left, r.top + 4, r.right, r.bottom - 4}, 4, kWhite, kControl);
        ui.text(kFilters[i], Ui::kBody, r, kWhite, sel || h ? kTextPrimary : kTextSecondary,
                DWRITE_TEXT_ALIGNMENT_CENTER);
        if (sel)
            ui.fill({fx + tw / 2 - 8, y + 36, fx + tw / 2 + 8, y + 39}, 1.5f, accent);
        ui.hot(r, kFilterId + i);
        fx += tw;
    }
    float bx = ui.buttonRight(L"Export", x + w, y + 20, kExportId, false);
    ui.buttonRight(L"Clear", bx - 8, y + 20, kClearId, false);
    y += 40 + 12;

    std::vector<const Line*> shownLines;
    for (const Line& l : lines_)
        if (shown(l))
            shownLines.push_back(&l);
    if (shownLines.empty()) {
        ui.card({x, y, x + w, y + 64});
        ui.text(!capture_.running() ? L"Start capturing to see what the drivers print."
                : filter_ == kAll   ? L"Nothing yet."
                                    : L"Nothing from the drivers yet. They print as programs use the GPU.",
                Ui::kBody, {x + 16, y, x + w - 16, y + 64}, kWhite, kTextSecondary);
        y += 64;
        return;
    }
    // A selection can go over lines, also past those in view.
    ui.runText(kLineRun, [this](int id, std::wstring* s) {
        if (lines_.empty() || id < kLineText)
            return false;
        size_t i = unsigned(id - kLineText) - lines_.front().seq % 100000000;
        if (i >= lines_.size() || !shown(lines_[i]))
            return false;
        *s = lines_[i].text;
        return true;
    });
    // The lines fill the card down to the window's bottom and scroll in it.
    D2D1_RECT_F card = {x, y, x + w, std::max(y + 120, ui.view - 40)};
    ui.card(card);
    ui.beginScroll(card, shownLines.size() * kRowH + 24);
    // Only the rows in view: the log can be long.
    size_t first = size_t(std::max(0.f, (ui.scroll - 12) / kRowH));
    float bottom = card.bottom + ui.scroll;
    for (size_t i = first; i < shownLines.size(); i++) {
        float ry = y + 12 + i * kRowH;
        if (ry > bottom)
            break;
        const Line* l = shownLines[i];
        if (l->level)
            ui.text(l->level == 2 ? L"" : L"", Ui::kIconSmall, {x + 12, ry, x + 30, ry + kRowH},
                    l->level == 2 ? kCritical : kCaution, 1, DWRITE_TEXT_ALIGNMENT_CENTER);
        ui.text(clock(l->time, true), Ui::kMono, {x + 38, ry, x + 142, ry + kRowH}, kWhite, kTextTertiary);
        ui.text(l->component.empty() ? l->process : l->component, Ui::kMono, {x + 146, ry, x + 230, ry + kRowH},
                kWhite, kTextSecondary);
        ui.selectable(kLineText + int(l->seq % 100000000), l->text, Ui::kMono, {x + 238, ry, x + w - 20, ry + kRowH},
                     kWhite, kTextPrimary, kLineRun);
    }
    ui.endScroll();
    y = card.bottom;
}

void Logs::click(int id)
{
    if (id >= kFilterId && id < kFilterId + 4) {
        filter_ = id - kFilterId;
    } else if (id == kCaptureId) {
        if (capture_.running())
            capture_.stop();
        else
            start();
    } else if (id == kClearId) {
        lines_.clear();
    } else if (id == kExportId) {
        exportLog();
    }
}

void Logs::exportLog()
{
    SYSTEMTIME t;
    GetLocalTime(&t);
    wchar_t file[MAX_PATH];
    swprintf(file, MAX_PATH, L"venera-log-%04u%02u%02u-%02u%02u%02u.txt", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute,
             t.wSecond);
    wchar_t docs[MAX_PATH] = L"";
    SHGetFolderPathW(nullptr, CSIDL_PERSONAL, nullptr, 0, docs);
    OPENFILENAMEW ofn = {sizeof(ofn)};
    ofn.hwndOwner = window_;
    ofn.lpstrFilter = L"Text files (*.txt)\0*.txt\0All files\0*.*\0";
    ofn.lpstrFile = file;
    ofn.nMaxFile = MAX_PATH;
    ofn.lpstrInitialDir = docs;
    ofn.lpstrDefExt = L"txt";
    ofn.Flags = OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST;
    if (!GetSaveFileNameW(&ofn))
        return;
    std::wstring text;
    for (const Line& l : lines_)
        text += formatLogLine(l) + L"\r\n";
    std::string utf8 = narrow(text);
    HANDLE f = CreateFileW(file, GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (f == INVALID_HANDLE_VALUE)
        return;
    DWORD written;
    WriteFile(f, utf8.data(), DWORD(utf8.size()), &written, nullptr);
    CloseHandle(f);
}

}  // namespace

std::unique_ptr<Page> makeLogs(HWND window) { return std::make_unique<Logs>(window); }

}  // namespace panel
