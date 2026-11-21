// SPDX-License-Identifier: MIT
// About: updates first, as Windows Update shows them (a card with where things stand and what to do, then the
// channel), then Venera itself (the logo, its version, who makes it, its license) and the open-source projects it is
// built on, with their licenses; a project's row opens its page. Updates come from GitHub (update.cpp): the page
// checks only when asked to (the button), downloads without administrator rights, and hands the file
// to venera-panel /update as administrator, which checks it again and starts the installer; the panel then closes,
// for the installer to replace it.
#include "panel.h"

#include <shellapi.h>

#include <atomic>
#include <algorithm>

namespace panel {

using namespace theme;

namespace {

constexpr float kHeroH = 104, kGap = 4, kRowH = 56;
constexpr UINT kChecked = kPageMessage + 5, kProgress = kPageMessage + 6, kDownloaded = kPageMessage + 7,
               kInstalled = kPageMessage + 8;

constexpr wchar_t kProject[] = L"https://github.com/Aleksanaa/venera";
constexpr wchar_t kTelegram[] = L"https://t.me/+pbvGcZWzH3RmNDA1";

struct Credit {
    const wchar_t* name; unsigned use; locale::Message license; const wchar_t* url;
};
// What the installer's files are built from.
const Credit kCredits[] = {
    {L"yttrium", IDS_CreditYttrium, L"BSD-3-Clause, MPL-2.0",
     L"https://github.com/arehnman/yttrium-virtio-gpu"},
    {L"Mesa", IDS_CreditMesa, L"MIT",
     L"https://github.com/arehnman/virtio-win-mesa"},
    {L"DXVK", IDS_CreditDxvk, L"Zlib", L"https://github.com/doitsujin/dxvk"},
    {L"vkd3d-proton", IDS_CreditVkd3d, L"LGPL-2.1-or-later",
     L"https://github.com/HansKristian-Work/vkd3d-proton"},
    {L"dxil-spirv", IDS_CreditDxil, L"MIT",
     L"https://github.com/HansKristian-Work/dxil-spirv"},
    {L"dxbc-spirv", IDS_CreditDxbc, L"MIT", L"https://github.com/doitsujin/dxbc-spirv"},
    {L"libdisplay-info", IDS_CreditDisplay, L"MIT",
     L"https://gitlab.freedesktop.org/emersion/libdisplay-info"},
    {L"DirectX-Headers", IDS_CreditDirectx, L"MIT", L"https://github.com/microsoft/DirectX-Headers"},
    {L"Vulkan-Headers", IDS_CreditVulkan, L"Apache-2.0 OR MIT", L"https://github.com/KhronosGroup/Vulkan-Headers"},
    {L"MinGW-w64", IDS_CreditMingw, {IDS_LicenseMingw},
     L"https://www.mingw-w64.org"},
    {L"GCC runtime and mcfgthread", IDS_CreditRuntime, {IDS_LicenseRuntime},
     L"https://github.com/lhmouse/mcfgthread"},
};
constexpr int kCreditCount = sizeof(kCredits) / sizeof(kCredits[0]);

enum : int { kCheckId = kPageId, kChannelId, kDownloadId, kInstallId, kCancelId, kGithubId, kTelegramId, kCreditId,
             kVersionText = kPageId + 100, kStatusText };

struct Downloaded {
    std::wstring path;
    locale::Message error;
};

// The exe's icon (panel.rc), `px` pixels square, as premultiplied BGRA for a Direct2D bitmap.
std::vector<UINT32> iconPixels(int px)
{
    std::vector<UINT32> out;
    HICON icon = static_cast<HICON>(
        LoadImageW(GetModuleHandleW(nullptr), MAKEINTRESOURCEW(1), IMAGE_ICON, px, px, LR_DEFAULTCOLOR));
    ICONINFO ii;
    if (!icon || !GetIconInfo(icon, &ii))
        return out;
    BITMAPINFO bi = {};
    bi.bmiHeader = {sizeof(BITMAPINFOHEADER), px, -px, 1, 32, BI_RGB};
    out.resize(size_t(px) * px);
    HDC dc = GetDC(nullptr);
    if (!ii.hbmColor || GetDIBits(dc, ii.hbmColor, 0, px, out.data(), &bi, DIB_RGB_COLORS) != px)
        out.clear();
    ReleaseDC(nullptr, dc);
    if (ii.hbmColor)
        DeleteObject(ii.hbmColor);
    DeleteObject(ii.hbmMask);
    DestroyIcon(icon);
    for (UINT32& p : out) {
        UINT32 a = p >> 24;
        p = a << 24 | ((p >> 16 & 255) * a / 255) << 16 | ((p >> 8 & 255) * a / 255) << 8 | (p & 255) * a / 255;
    }
    return out;
}

std::wstring megabytes(ULONGLONG bytes)
{
    wchar_t s[32];
    swprintf(s, 32, L"%.1f MB", bytes / 1048576.0);
    return s;
}

class About : public Page {
public:
    explicit About(HWND window) : window_(window) {}
    ~About() override
    {
        if (cancel_)
            *cancel_ = true;
    }
    const wchar_t* title() const override { return locale::text(IDS_About); }
    const wchar_t* icon() const override { return L""; }
    void draw(Ui& ui, float x, float w, float& y) override;
    void click(int id) override;
    bool message(UINT msg, WPARAM wp, LPARAM lp) override;
    bool animating() const override { return state_ == kChecking || state_ == kDownloading || state_ == kInstalling; }

private:
    enum State { kIdle, kChecking, kUpToDate, kAvailable, kDownloading, kReady, kInstalling, kFailed };

    void check();
    void download();
    void install(const std::wstring& path);
    void fail(locale::Message title, locale::Message error)
    {
        state_ = kFailed;
        errorTitle_ = title;
        error_ = error;
    }
    void drawUpdate(Ui& ui, float x, float w, float& y);
    void drawAbout(Ui& ui, float x, float w, float& y);
    void drawIcon(Ui& ui, float x, float y, float size);
    const wchar_t* newest() const { return channel_ == Channel::kDevelop ? locale::text(IDS_DevelopNoun) : locale::text(IDS_ReleaseNoun); }

    HWND window_;
    State state_ = kIdle;
    Channel channel_ = buildChannel();  // or the other for this session: what it installs then follows that one
    std::wstring installed_ = installedVersion();
    Update update_;
    locale::Message errorTitle_, error_;
    std::wstring checkedAt_;
    std::wstring downloaded_;  // kReady: the file
    ULONGLONG done_ = 0;
    unsigned gen_ = 0;  // the operation going on: what an older one posts is dropped
    std::shared_ptr<std::atomic<bool>> cancel_;
    std::vector<UINT32> icon_;
    int iconPx_ = 0;
};

// ---- updating ---------------------------------------------------------------------------------------------------------

void About::check()
{
    state_ = kChecking;
    installed_ = installedVersion();
    unsigned gen = ++gen_;
    HWND window = window_;
    Channel channel = channel_;
    std::thread([window, gen, channel] {
        auto* u = new Update(findUpdate(channel));
        if (!PostMessageW(window, kChecked, gen, reinterpret_cast<LPARAM>(u)))
            delete u;
    }).detach();
}

void About::download()
{
    state_ = kDownloading;
    done_ = 0;
    unsigned gen = ++gen_;
    if (cancel_)
        *cancel_ = true;
    cancel_ = std::make_shared<std::atomic<bool>>(false);
    auto cancel = cancel_;
    HWND window = window_;
    Update u = update_;
    std::thread([window, gen, cancel, u] {
        auto* r = new Downloaded{downloadPath(u)};
        DWORD posted = 0;
        r->error = downloadUpdate(u, r->path, [&](ULONGLONG done, ULONGLONG) {
            if (GetTickCount() - posted >= 50) {
                posted = GetTickCount();
                PostMessageW(window, kProgress, gen, LPARAM(done));
            }
            return !*cancel;
        });
        if (!PostMessageW(window, kDownloaded, gen, reinterpret_cast<LPARAM>(r)))
            delete r;
    }).detach();
}

// venera-panel /update as administrator, which says itself what went wrong; this panel closes once it started the
// installer. Run from the click on Install: a UAC prompt asked for from elsewhere may wait behind the windows.
void About::install(const std::wstring& path)
{
    state_ = kInstalling;
    RedrawWindow(window_, nullptr, nullptr, RDW_INVALIDATE | RDW_UPDATENOW);  // before UAC holds this thread
    std::wstring args = std::wstring(L"/update ") + channelName(channel_) + L" /from \"" + path + L"\"";
    // it has no window but its error message box, which SW_HIDE would hide
    HANDLE process = startSelf(window_, args, SW_SHOWNORMAL);
    if (!process) {
        fail({IDS_UpdateNotStarted}, {GetLastError() == ERROR_CANCELLED ? IDS_UpdateAdminDeclined : IDS_UpdateAdminFailed});
        return;
    }
    unsigned gen = ++gen_;
    HWND window = window_;
    std::thread([window, gen, process] {
        WaitForSingleObject(process, INFINITE);
        DWORD code = 1;
        GetExitCodeProcess(process, &code);
        CloseHandle(process);
        PostMessageW(window, kInstalled, gen, code);
    }).detach();
}

bool About::message(UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == kChecked) {
        std::unique_ptr<Update> u(reinterpret_cast<Update*>(lp));
        if (wp != gen_)
            return false;
        update_ = std::move(*u);
        wchar_t time[64] = L"";
        GetTimeFormatEx(LOCALE_NAME_USER_DEFAULT, TIME_NOSECONDS, nullptr, nullptr, time, 64);
        checkedAt_ = time;
        if (!update_.error.empty())
            fail({IDS_UpdateCheckFailed}, update_.error);
        else if (installed_.empty() || compareVersions(update_.version, installed_) > 0)
            state_ = kAvailable;
        else
            state_ = kUpToDate;
        return true;
    }
    if (msg == kProgress) {
        if (wp != gen_)
            return false;
        done_ = ULONGLONG(lp);
        return true;
    }
    if (msg == kDownloaded) {
        std::unique_ptr<Downloaded> r(reinterpret_cast<Downloaded*>(lp));
        if (wp != gen_)
            return false;
        if (!r->error.empty()) {
            fail({IDS_DownloadFailed, {update_.version}}, r->error);
        } else {
            state_ = kReady;
            downloaded_ = r->path;
        }
        return true;
    }
    if (msg == kInstalled) {
        if (wp != gen_)
            return false;
        if (lp == 0)
            PostMessageW(window_, WM_CLOSE, 0, 0);
        else
            fail({IDS_UpdateNotStarted}, {IDS_UpdateProcessFailed});
        return true;
    }
    return false;
}

void About::click(int id)
{
    if (id == kCheckId) {
        check();
    } else if (id == kDownloadId) {
        download();
    } else if (id == kInstallId) {
        install(downloaded_);
    } else if (id == kCancelId) {
        if (cancel_)
            *cancel_ = true;
        ++gen_;
        state_ = kAvailable;
    } else if (id == kChannelId) {
        // The choices in a menu, as a ComboBox's dropdown, at the pointer.
        HMENU menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING | (channel_ == Channel::kRelease ? MF_CHECKED : 0), 1, locale::text(IDS_Release));
        AppendMenuW(menu, MF_STRING | (channel_ == Channel::kDevelop ? MF_CHECKED : 0), 2, locale::text(IDS_Develop));
        POINT pt;
        GetCursorPos(&pt);
        int c = TrackPopupMenu(menu, TPM_RETURNCMD, pt.x, pt.y, 0, window_, nullptr);
        DestroyMenu(menu);
        Channel chosen = c == 2 ? Channel::kDevelop : Channel::kRelease;
        if (c > 0 && chosen != channel_) {
            channel_ = chosen;
            ++gen_;  // a check of the other channel going on is dropped
            state_ = kIdle;
        }
    } else if (id == kGithubId) {
        ShellExecuteW(window_, nullptr, kProject, nullptr, nullptr, SW_SHOWNORMAL);
    } else if (id == kTelegramId) {
        ShellExecuteW(window_, nullptr, kTelegram, nullptr, nullptr, SW_SHOWNORMAL);
    } else if (id >= kCreditId && id < kCreditId + kCreditCount) {
        ShellExecuteW(window_, nullptr, kCredits[id - kCreditId].url, nullptr, nullptr, SW_SHOWNORMAL);
    }
}

// ---- drawing --------------------------------------------------------------------------------------------------------

void About::draw(Ui& ui, float x, float w, float& y)
{
    drawUpdate(ui, x, w, y);
    y += 24;
    drawAbout(ui, x, w, y);
}

// Where things stand, with what can be done on the right, then the channel.
void About::drawUpdate(Ui& ui, float x, float w, float& y)
{
    std::wstring title, sub;
    const wchar_t* glyph = L"";  // Sync
    UINT32 glyphColor = accent;
    std::wstring have = installed_.empty() ? locale::text(IDS_VeneraNotInstalled) : locale::format(IDS_InstalledVersion, {installed_});
    switch (state_) {
    case kIdle:
        title = installed_.empty() ? locale::text(IDS_VeneraNotInstalled) : L"Venera " + installed_;
        sub = locale::text(channel_ == Channel::kDevelop ? IDS_CheckDevelop : IDS_CheckRelease);
        break;
    case kChecking:
        title = locale::text(IDS_CheckingUpdates);
        sub = locale::text(channel_ == Channel::kDevelop ? IDS_AskingDevelop : IDS_AskingRelease);
        break;
    case kUpToDate:
        title = locale::text(IDS_UpToDate);
        glyph = L"";  // Completed
        sub = locale::format(IDS_UpdateCurrent, {have, newest(), update_.version, checkedAt_}) +
              (update_.note.empty() ? L"" : L" · " + update_.note.resolve());
        break;
    case kAvailable:
        title = locale::format(IDS_UpdateAvailable, {update_.version});
        sub = have + L" · " + megabytes(update_.size) + (update_.note.empty() ? L"" : L" · " + update_.note.resolve());
        break;
    case kDownloading:
        title = locale::format(IDS_DownloadingVersion, {update_.version});
        sub = locale::format(IDS_DownloadProgress, {megabytes(done_), megabytes(update_.size)});
        break;
    case kReady:
        title = locale::format(IDS_VersionReady, {update_.version});
        sub = locale::text(IDS_InstallDescription);
        break;
    case kInstalling:
        title = locale::format(IDS_InstallingVersion, {update_.version});
        sub = locale::text(IDS_WaitingInstaller);
        break;
    case kFailed:
        title = errorTitle_.resolve();
        glyph = L"";  // Error
        glyphColor = kCritical;
        sub = error_.resolve();
        break;
    }

    const wchar_t* action = state_ == kAvailable ? locale::text(IDS_Download) :
        state_ == kReady ? locale::text(IDS_Install) : state_ == kDownloading ? locale::text(IDS_Cancel) :
        state_ == kFailed ? locale::text(IDS_TryAgain) : locale::text(IDS_CheckUpdates);
    float actionWidth = state_ == kChecking || state_ == kInstalling ? 28.f : ui.buttonWidth(action);
    if (state_ == kDownloading) actionWidth += 44;
    float textWidth = std::max(100.f, w - 144 - actionWidth);
    auto heading = ui.wrap(title, Ui::kSubtitle, textWidth);
    auto description = ui.wrap(sub, Ui::kBody, textWidth);
    float heroH = std::max(kHeroH, heading.height + description.height + 48);
    ui.card({x, y, x + w, y + heroH});
    ui.text(glyph, Ui::kGlyph, {x + 24, y, x + 72, y + heroH}, glyphColor, 1, DWRITE_TEXT_ALIGNMENT_CENTER);
    float right = x + w - 24, cy = y + heroH / 2;
    if (state_ == kAvailable) {
        right = ui.buttonRight(locale::text(IDS_Download), right, cy, kDownloadId, true);
    } else if (state_ == kReady) {
        right = ui.buttonRight(locale::text(IDS_Install), right, cy, kInstallId, true);
    } else if (state_ == kDownloading) {
        right = ui.buttonRight(locale::text(IDS_Cancel), right, cy, kCancelId, false) - 16 - 28;
        ui.progressRing(right + 14, cy, 28, update_.size ? float(done_) / update_.size : -1);
    } else if (state_ == kChecking || state_ == kInstalling) {
        right -= 28;
        ui.progressRing(right + 14, cy, 28, -1);
    } else {
        const wchar_t* label = state_ == kFailed ? locale::text(IDS_TryAgain) : locale::text(IDS_CheckUpdates);
        right = ui.buttonRight(label, right, cy, kCheckId, false);
    }
    ui.text(heading, x + 96, y + 20);
    ui.selectable(kStatusText, description, x + 96, y + 28 + heading.height, kWhite, kTextSecondary);
    y += heroH + kGap;

    float ccy = ui.settingsCard(x, y, w, L"" /* Switch */, locale::text(IDS_UpdateChannel),
                                locale::text(IDS_UpdateChannelDescription));
    ui.comboBox(x + w - 16, ccy, kChannelId, {locale::text(IDS_Release), locale::text(IDS_Develop)}, channel_ == Channel::kDevelop,
                state_ != kDownloading && state_ != kInstalling);
}

// From the icon's own size for the window's DPI, drawn 1:1.
void About::drawIcon(Ui& ui, float x, float y, float size)
{
    int px = int(size * GetDpiForWindow(window_) / 96.f + 0.5f);
    if (px != iconPx_) {
        icon_ = iconPixels(px);
        iconPx_ = px;
    }
    if (!icon_.empty())
        ui.image(icon_.data(), px, x, y);
}

void About::drawAbout(Ui& ui, float x, float w, float& y)
{
    // the logo, the name and what it is, with its links; under a divider who makes it and its license
    struct Row {
        const wchar_t* key;
        std::wstring value;
    };
    const Row rows[] = {
        {locale::text(IDS_Developer), L"Aleksana"},
        {locale::text(IDS_Contributors), L"MiRinChan, gjz010, xddxdd"},
        {locale::text(IDS_License), L"MIT"},
    };
    constexpr int kRows = sizeof(rows) / sizeof(rows[0]);
    float topH = 144, h = topH + 1 + 12 + kRows * 32 + 12;
    ui.card({x, y, x + w, y + h});
    drawIcon(ui, x + 32, y + 32, 80);
    float tx = x + 140;
    float right = ui.buttonRight(L"Telegram", x + w - 24, y + 72, kTelegramId, false);
    right = ui.buttonRight(L"GitHub", right - 8, y + 72, kGithubId, false) - 24;
    ui.text(L"Venera", Ui::kTitle, {tx, y + 26, right, y + 66});
    ui.text(locale::text(IDS_ProductDescription), Ui::kBody,
            {tx, y + 68, right, y + 90}, kWhite, kTextSecondary);
    const std::wstring version =
        locale::format(buildChannel() == Channel::kDevelop ? IDS_DevelopVersion : IDS_Version, {installed_});
    ui.selectable(kVersionText, installed_.empty() ? locale::text(IDS_NotInstalled) : version, Ui::kCaption,
                  {tx, y + 92, right, y + 112}, kWhite, kTextTertiary);
    ui.divider(x, w, y + topH);
    float ry = y + topH + 1 + 12;
    for (const Row& r : rows) {
        ui.text(r.key, Ui::kBody, {x + 32, ry, x + 32 + 180, ry + 32}, kWhite, kTextSecondary);
        ui.text(r.value, Ui::kBody, {x + 32 + 180, ry, x + w - 16, ry + 32});
        ry += 32;
    }
    y += h;

    // the projects, one row each, as Settings' related links: the license and an arrow out on the right
    ui.sectionHeader(locale::text(IDS_OpenSourceProjects), x, y);
    h = kCreditCount * kRowH;
    ui.card({x, y, x + w, y + h});
    for (int i = 0; i < kCreditCount; i++) {
        const Credit& c = kCredits[i];
        float top = y + i * kRowH;
        int id = kCreditId + i;
        if (i)
            ui.divider(x, w, top);
        auto [over, pressed] = ui.pointer(id);
        if (over)
            ui.fill({x + 1, top + 1, x + w - 1, top + kRowH - 1}, 3, kWhite,
                    pressed ? kControlPressed : kCardHover - kCardFill);
        ui.hot({x, top, x + w, top + kRowH}, id);
        float licenseW = 220;
        ui.titleDesc(x + 16, x + w - licenseW - 56, top + kRowH / 2, c.name, locale::text(c.use));
        ui.text(c.license.resolve(), Ui::kBody, {x + w - licenseW - 48, top, x + w - 48, top + kRowH}, kWhite, kTextSecondary,
                DWRITE_TEXT_ALIGNMENT_TRAILING);
        ui.text(L"" /* OpenInNewWindow */, Ui::kIconSmall, {x + w - 40, top, x + w - 16, top + kRowH}, kWhite,
                kTextSecondary, DWRITE_TEXT_ALIGNMENT_CENTER);
    }
    y += h;
}

}  // namespace

std::unique_ptr<Page> makeAbout(HWND window) { return std::make_unique<About>(window); }

}  // namespace panel
