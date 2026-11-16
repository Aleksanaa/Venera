// SPDX-License-Identifier: MIT
#include "debugchannel.h"
#include <shellapi.h>
#include <shlobj.h>
#include <sstream>
#include <cerrno>
#include <algorithm>

namespace panel {
namespace {
std::wstring healthText(const std::wstring& value, bool listener = false)
{
    const std::pair<const wchar_t*, unsigned> states[] = {
        {L"installed", IDS_DebugInstalled}, {L"missing", IDS_DebugMissing},
        {L"present", IDS_DebugPresent}, {L"running", IDS_DebugRunning},
        {L"retrying", IDS_DebugRetrying}, {L"probing", IDS_DebugProbing},
        {L"connected", IDS_HostConnected}, {L"waiting", IDS_DebugWaiting},
        {L"disconnected", IDS_HostDisconnected}, {L"connecting", IDS_HostConnecting},
        {L"Unknown", IDS_DebugUnknown},
    };
    size_t end = value.find_first_of(L" (—");
    std::wstring token = value.substr(0, end);
    if(listener && token == L"present")
        return std::wstring(locale::text(IDS_SocketListening)) + (end == std::wstring::npos ? L"" : value.substr(end));
    for (const auto& state : states)
        if (token == state.first)
            return std::wstring(locale::text(state.second)) + (end == std::wstring::npos ? L"" : value.substr(end));
    return value;
}
constexpr UINT kResult=kPageMessage+30;
constexpr UINT_PTR kRefresh=kPageTimer+30;
enum { Refresh=kPageId, Forward, Filter, Reset, Install, Details };
struct FilterForm {
    std::string json;
    debugjson::Value current;
    std::wstring summary;
};
std::wstring selected(const debugjson::Value& values) {
    std::wstring text;
    for(const auto& value:values.items) {
        if(!text.empty()) text+=L", "; text+=widen(value.text);
    }
    return text.empty()?locale::text(IDS_All):text;
}
std::wstring filterSummary(const debugjson::Value& filter) {
    std::wstring summary=locale::format(IDS_FilterSummary, {selected(filter["components"]), selected(filter["levels"])});
    for(const auto& target:filter["targets"].items) {
        if(target["kind"].text=="exe_path") summary+=L"\n"+locale::format(IDS_ProgramTarget, {widen(target["path"].text)});
        else summary+=L"\n"+locale::format(target["include_descendants"].boolean ? IDS_ProcessTreeTarget : IDS_ProcessTarget,
            {widen(target["pid"].text)});
    }
    if(filter["targets"].items.empty()) summary+=L"\n"+std::wstring(locale::text(IDS_AllPrograms));
    return summary;
}
std::wstring edit(HWND window,int id) {
    HWND control=GetDlgItem(window,id); int n=GetWindowTextLengthW(control);
    std::wstring s(n+1,L'\0'); GetWindowTextW(control,s.data(),n+1); s.resize(n); return s;
}
debugjson::Json list(const std::wstring& text) {
    auto out=debugjson::Json::array();
    if(text==locale::text(IDS_All) || text.empty()) return out;
    std::wistringstream stream(text); std::wstring item;
    while(std::getline(stream,item,L',')) {
        size_t first=item.find_first_not_of(L" \t"), last=item.find_last_not_of(L" \t");
        if(first==std::wstring::npos) continue;
        out.push_back(narrow(item.substr(first,last-first+1)));
    }
    return out;
}
bool integer(const std::wstring& s, unsigned long long& n) {
    if(s.empty()||s.find_first_not_of(L"0123456789")!=std::wstring::npos) return false;
    errno=0; wchar_t* end; n=wcstoull(s.c_str(),&end,10); return errno==0&&!*end;
}
INT_PTR CALLBACK filterProc(HWND window,UINT msg,WPARAM wp,LPARAM lp) {
    if(msg==WM_INITDIALOG) {
        SetWindowLongPtrW(window,DWLP_USER,lp);
        auto* form=reinterpret_cast<FilterForm*>(lp);
        for(const wchar_t* s:{locale::text(IDS_CurrentPrograms),locale::text(IDS_AllPrograms),locale::text(IDS_ProcessDescendants),locale::text(IDS_ExecutablePath)})
            SendDlgItemMessageW(window,503,CB_ADDSTRING,0,reinterpret_cast<LPARAM>(s));
        SendDlgItemMessageW(window,503,CB_SETCURSEL,0,0);
        for(const wchar_t* s:{locale::text(IDS_All),L"venera_d3d11",L"venera_d3d12",L"mesa",L"venus",L"dxvk",L"vkd3d",L"vkd3d-shader",L"kernel",L"service"})
            SendDlgItemMessageW(window,501,CB_ADDSTRING,0,reinterpret_cast<LPARAM>(s));
        for(const wchar_t* s:{locale::text(IDS_All),L"trace",L"debug",L"info",L"warning",L"error",L"fatal",L"unknown"})
            SendDlgItemMessageW(window,502,CB_ADDSTRING,0,reinterpret_cast<LPARAM>(s));
        SetDlgItemTextW(window,501,selected(form->current["components"]).c_str());
        SetDlgItemTextW(window,502,selected(form->current["levels"]).c_str());
        SetDlgItemTextW(window,506,form->summary.c_str());
        const auto& targets=form->current["targets"].items;
        if(targets.size()==1) {
            const auto& target=targets.front();
            SetDlgItemTextW(window,504,widen(target[target["kind"].text=="exe_path"?"path":"pid"].text).c_str());
        }
        EnableWindow(GetDlgItem(window,504),FALSE);
        return TRUE;
    }
    if(msg==WM_COMMAND) {
        if(LOWORD(wp)==503 && HIWORD(wp)==CBN_SELCHANGE) {
            int mode=SendDlgItemMessageW(window,503,CB_GETCURSEL,0,0);
            EnableWindow(GetDlgItem(window,504),mode==2||mode==3);
            return TRUE;
        }
        if(LOWORD(wp)==IDCANCEL) { EndDialog(window,0); return TRUE; }
        if(LOWORD(wp)==IDOK) {
            auto* form=reinterpret_cast<FilterForm*>(GetWindowLongPtrW(window,DWLP_USER));
            auto targets=form->current["targets"].json; std::wstring error;
            int mode=SendDlgItemMessageW(window,503,CB_GETCURSEL,0,0);
            auto target=edit(window,504); unsigned long long n=0;
            if(mode==1) targets=debugjson::Json::array();
            if(mode==2) {
                if(!integer(target,n)||!n||n>MAXDWORD) error=locale::text(IDS_EnterProcess);
                else {
                    HANDLE process=OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,FALSE,DWORD(n));
                    FILETIME created,exited,kernel,user;
                    if(!process||!GetProcessTimes(process,&created,&exited,&kernel,&user))
                        error=locale::text(IDS_ProcessTimeFailed);
                    else {
                        ULARGE_INTEGER time; time.LowPart=created.dwLowDateTime; time.HighPart=created.dwHighDateTime;
                        targets=debugjson::Json::array({{{"kind","process_instance"},{"pid",n},
                            {"start_time_filetime",std::to_string(time.QuadPart)},{"include_descendants",true}}});
                    }
                    if(process) CloseHandle(process);
                }
            } else if(mode==3) {
                if(target.size()<3 || !((target[1]==L':'&&(target[2]==L'\\'||target[2]==L'/')) || target.substr(0,2)==L"\\\\"))
                    error=locale::text(IDS_EnterPath);
                else targets=debugjson::Json::array({{{"kind","exe_path"},{"path",narrow(target)}}});
            }
            std::wstring duration=edit(window,505); unsigned long long minutes=0;
            if(!duration.empty() && (!integer(duration,minutes)||!minutes||minutes>~0ull/60000))
                error=locale::text(IDS_EnterMinutes);
            if(!error.empty()) { MessageBoxW(window,error.c_str(),locale::text(IDS_DebugFilter),MB_OK|MB_ICONWARNING); return TRUE; }
            debugjson::Json request={{"filter",{{"components",list(edit(window,501))},
                {"levels",list(edit(window,502))},{"targets",targets}}}};
            if(minutes) request["duration_ms"]=minutes*60000;
            form->json=request.dump(); EndDialog(window,1); return TRUE;
        }
    }
    return FALSE;
}

class DebugPage : public Page {
public:
    explicit DebugPage(HWND window):window_(window) { SetTimer(window_,kRefresh,2500,nullptr); launch(); }
    ~DebugPage() override { KillTimer(window_,kRefresh); if(worker_.joinable()) worker_.join(); }
    const wchar_t* title() const override { return locale::text(IDS_DebugChannel); }
    const wchar_t* icon() const override { return L""; }
    void draw(Ui& ui,float x,float w,float& y) override {
        int textId=kPageId+100;
        auto row=[&](const wchar_t* icon,const std::wstring& title,const std::wstring& desc,float reserve=0.f) {
            auto heading=ui.wrap(title,Ui::kBody,w-72-reserve);
            auto body=ui.wrap(desc,Ui::kCaption,w-72-reserve);
            float h=std::max(68.f,heading.height+body.height+28);
            ui.card({x,y,x+w,y+h});
            ui.text(icon,Ui::kIconLarge,{x+16,y,x+36,y+h},theme::kWhite,1,DWRITE_TEXT_ALIGNMENT_CENTER);
            float top=y+(h-heading.height-body.height-4)/2;
            ui.selectable(textId++,heading,x+56,top);
            ui.selectable(textId++,body,x+56,top+heading.height+4,theme::kWhite,theme::kTextSecondary);
            float cy=y+h/2; y+=h+4; return cy;
        };
        float cy=row(L"\uE724",locale::text(IDS_ForwardDebug),locale::text(IDS_HostDescription),100);
        if(status_.valid) ui.toggle(x+w-20,cy,Forward,status_.forwarding,!applying_);
        if(!status_.error.empty())
            ui.infoBar(x,w,y,Ui::Severity::kError,locale::text(IDS_DebugService),
                stale_ ? locale::format(IDS_StaleError, {status_.error.resolve()}) : status_.error.resolve());
        if(applying_)
            ui.infoBar(x,w,y,Ui::Severity::kInformational,L"",locale::text(IDS_ApplyingChanges));
        std::wstring connection;
        if(status_.valid) {
            connection=healthText(status_.host);
            if(stale_) connection=locale::format(IDS_LastStatus, {connection});
        } else connection=busy_?locale::text(IDS_Checking):locale::text(IDS_StatusUnavailable);
        row(L"\uE703",locale::text(IDS_HostConnection),connection);
        if(!status_.valid)
            row(L"\uE7B8",locale::text(IDS_SocketDriver),
                healthText(status_.driver.empty()?L"Unknown":status_.driver));

        float refreshW=ui.measure(locale::text(IDS_Refresh),Ui::kBody)+40;
        D2D1_RECT_F refreshRect={x,y,x+refreshW,y+32};
        if(ui.pointer(Refresh).over) ui.fill(refreshRect,4,theme::kWhite,theme::kControlHover);
        ui.text(L"\uE72C",Ui::kIcon,{x+8,y,x+24,y+32});
        ui.text(locale::text(IDS_Refresh),Ui::kBody,{x+32,y,x+refreshW,y+32});
        ui.hot(refreshRect,Refresh); y+=40;
        if(status_.valid) {
            row(L"\uE71C",locale::text(IDS_ReceiveFilter),locale::format(IDS_ReceiveFilterDescription, {filterSummary(status_.effectiveFilter)}));
            float bw=ui.button(locale::text(IDS_EditFilter),x,y,Filter,true);
            ui.button(locale::text(IDS_ResetFilter),x+bw+8,y,Reset,false); y+=40;
            if(!status_.expires.empty()) row(L"\uE81C",locale::text(IDS_FilterExpires),status_.expires);
            const wchar_t* icons[]={L"\uE7B8",L"\uE772",L"\uE71B",L"\uE703"};
            const wchar_t* titles[]={locale::text(IDS_SocketDriver),locale::text(IDS_Controller),locale::text(IDS_DebugPort),locale::text(IDS_HostConnection)};
            const std::wstring* values[]={&status_.driver,&status_.controller,&status_.port,&status_.host};
            Ui::Paragraph descriptions[4];
            float heights[4],bodyH=16;
            for(int i=0;i<4;i++) {
                descriptions[i]=ui.wrap(healthText(*values[i],i==2),Ui::kCaption,w-72);
                heights[i]=std::max(60.f,descriptions[i].height+36); bodyH+=heights[i];
            }
            float h=ui.expander(x,y,w,Details,detailsOpen_,bodyH);
            ui.text(L"\uE71B",Ui::kIconLarge,{x+16,y,x+36,y+64});
            ui.titleDesc(x+56,x+w-52,y+32,locale::text(IDS_ConnectionDetails),stale_?locale::text(IDS_LastSuccessfulStatus):locale::text(IDS_DriverDevicesHost));
            if(detailsOpen_) {
                float top=y+73;
                for(int i=0;i<4;i++) {
                    ui.text(icons[i],Ui::kIconLarge,{x+16,top,x+36,top+heights[i]});
                    ui.text(titles[i],Ui::kBody,{x+56,top+6,x+w-16,top+26});
                    ui.selectable(kPageId+200+i,descriptions[i],x+56,top+30,theme::kWhite,theme::kTextSecondary);
                    top+=heights[i];
                }
            }
            y+=h+4;
        }
        if(driverMissing_) { ui.button(locale::text(IDS_InstallSocket),x,y,Install,true); y+=40; }
    }
    void click(int id) override {
        if(id==Details) { detailsOpen_=!detailsOpen_; return; }
        if(busy_) return;
        if(id==Refresh) launch();
        if(id==Forward||id==Filter||id==Reset) {
            if(!elevated()) {
                if(!restartElevated(window_,6)) status_.error={IDS_AdminApprovalFailed};
                return;
            }
            if(id==Forward) launch({L"forwarding",status_.forwarding?L"off":L"on"});
            if(id==Reset) launch({L"filter-reset"});
            if(id==Filter) {
                FilterForm form;
                form.current=status_.effectiveFilter;
                form.summary=filterSummary(status_.effectiveFilter);
                HMODULE module=GetModuleHandleW(nullptr);
                HRSRC resource=FindResourceExW(module,RT_DIALOG,MAKEINTRESOURCEW(500),locale::resourceLanguage());
                const auto* dialog=static_cast<const DLGTEMPLATE*>(LockResource(LoadResource(module,resource)));
                if(dialog && DialogBoxIndirectParamW(module,dialog,window_,filterProc,reinterpret_cast<LPARAM>(&form))==1)
                    launch({},form.json);
            }
        }
        if(id==Install) {
            wchar_t folder[MAX_PATH];
            if(FAILED(SHGetFolderPathW(nullptr,CSIDL_PROGRAM_FILES,nullptr,SHGFP_TYPE_CURRENT,folder))) {
                status_.error={IDS_InstallationFolderFailed}; return;
            }
            std::wstring setup=std::wstring(folder)+L"\\Venera\\venera-setup.exe";
            SHELLEXECUTEINFOW info={sizeof(info)}; info.lpVerb=L"runas"; info.lpFile=setup.c_str();
            info.lpParameters=L"/viosock"; info.hwnd=window_; info.nShow=SW_SHOWNORMAL;
            if(!ShellExecuteExW(&info)) status_.error={IDS_SocketSetupFailed, {std::to_wstring(GetLastError())}};
        }
    }
    bool message(UINT msg,WPARAM wp,LPARAM) override {
        if(msg==WM_TIMER&&wp==kRefresh) { if(!busy_) launch(); return true; }
        if(msg!=kResult) return false;
        if(worker_.joinable()) worker_.join();
        {
            std::lock_guard<std::mutex> lock(mutex_);
            driverMissing_=pending_.driver==L"missing";
            if(pending_.valid) { status_=std::move(pending_); stale_=false; }
            else { status_.driver=std::move(pending_.driver); status_.error=std::move(pending_.error); stale_=status_.valid; }
        }
        busy_=false; applying_=false;
        return true;
    }
private:
    void launch(std::vector<std::wstring> args={L"status",L"--json"},std::string filter={}) {
        if(worker_.joinable()) worker_.join(); busy_=true;
        applying_=!filter.empty() || args!=std::vector<std::wstring>{L"status",L"--json"};
        worker_=std::thread([this,args=std::move(args),filter=std::move(filter)]() mutable {
            DebugStatus result; std::wstring file;
            if(!filter.empty()) {
                wchar_t temp[MAX_PATH],name[MAX_PATH];
                if(GetTempPathW(MAX_PATH,temp)&&GetTempFileNameW(temp,L"vdf",0,name)) {
                    file=name; HANDLE f=CreateFileW(file.c_str(),GENERIC_WRITE,0,nullptr,TRUNCATE_EXISTING,FILE_ATTRIBUTE_TEMPORARY,nullptr);
                    DWORD written=0;
                    if(f==INVALID_HANDLE_VALUE||!WriteFile(f,filter.data(),DWORD(filter.size()),&written,nullptr)||written!=filter.size())
                        result.error={IDS_FilterSaveFailed};
                    if(f!=INVALID_HANDLE_VALUE) CloseHandle(f);
                    args={L"filter-set",L"--json",file};
                } else result.error={IDS_FilterCreateFailed};
            }
            if(result.error.empty()) result=queryDebug(args);
            if(!result.valid) {
                result.driver=L"Unknown";
                SC_HANDLE manager=OpenSCManagerW(nullptr,nullptr,SC_MANAGER_CONNECT);
                if(manager) {
                    SC_HANDLE driver=OpenServiceW(manager,L"VirtioSocket",SERVICE_QUERY_STATUS);
                    if(driver) { result.driver=L"installed"; CloseServiceHandle(driver); }
                    else if(GetLastError()==ERROR_SERVICE_DOES_NOT_EXIST) result.driver=L"missing";
                    CloseServiceHandle(manager);
                }
            }
            if(!file.empty()) DeleteFileW(file.c_str());
            { std::lock_guard<std::mutex> lock(mutex_); pending_=std::move(result); }
            PostMessageW(window_,kResult,0,0);
        });
    }
    HWND window_;
    bool busy_=false,applying_=false,driverMissing_=false,stale_=false,detailsOpen_=false;
    DebugStatus status_,pending_;
    std::thread worker_;
    std::mutex mutex_;
};
}
std::unique_ptr<Page> makeDebug(HWND window) { return std::make_unique<DebugPage>(window); }
}
