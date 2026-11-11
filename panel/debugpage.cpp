// SPDX-License-Identifier: MIT
#include "debugchannel.h"
#include <shellapi.h>
#include <shlobj.h>
#include <sstream>
#include <cerrno>

namespace panel {
namespace {
constexpr UINT kResult=kPageMessage+30;
constexpr UINT_PTR kRefresh=kPageTimer+30;
enum { Refresh=kPageId, Forward, Filter, Reset, Install };
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
    return text.empty()?L"All":text;
}
std::wstring edit(HWND window,int id) {
    HWND control=GetDlgItem(window,id); int n=GetWindowTextLengthW(control);
    std::wstring s(n+1,L'\0'); GetWindowTextW(control,s.data(),n+1); s.resize(n); return s;
}
std::string list(const std::wstring& text) {
    if(text==L"All" || text.empty()) return "[]";
    std::wistringstream stream(text); std::wstring item; std::string out="[";
    while(std::getline(stream,item,L',')) {
        size_t first=item.find_first_not_of(L" \t"), last=item.find_last_not_of(L" \t");
        if(first==std::wstring::npos) continue;
        if(out.size()>1) out+=','; out+=debugJsonString(item.substr(first,last-first+1));
    }
    return out+"]";
}
bool integer(const std::wstring& s, unsigned long long& n) {
    if(s.empty()||s.find_first_not_of(L"0123456789")!=std::wstring::npos) return false;
    errno=0; wchar_t* end; n=wcstoull(s.c_str(),&end,10); return errno==0&&!*end;
}
INT_PTR CALLBACK filterProc(HWND window,UINT msg,WPARAM wp,LPARAM lp) {
    if(msg==WM_INITDIALOG) {
        SetWindowLongPtrW(window,DWLP_USER,lp);
        auto* form=reinterpret_cast<FilterForm*>(lp);
        for(const wchar_t* s:{L"Current programs",L"All programs",L"Process and descendants",L"Executable path"})
            SendDlgItemMessageW(window,503,CB_ADDSTRING,0,reinterpret_cast<LPARAM>(s));
        SendDlgItemMessageW(window,503,CB_SETCURSEL,0,0);
        for(const wchar_t* s:{L"All",L"venera_d3d11",L"venera_d3d12",L"mesa",L"venus",L"dxvk",L"vkd3d",L"vkd3d-shader",L"kernel",L"service"})
            SendDlgItemMessageW(window,501,CB_ADDSTRING,0,reinterpret_cast<LPARAM>(s));
        for(const wchar_t* s:{L"All",L"trace",L"debug",L"info",L"warning",L"error",L"fatal",L"unknown"})
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
            std::string targets=debugjson::encode(form->current["targets"]); std::wstring error;
            int mode=SendDlgItemMessageW(window,503,CB_GETCURSEL,0,0);
            auto target=edit(window,504); unsigned long long n=0;
            if(mode==1) targets="[]";
            if(mode==2) {
                if(!integer(target,n)||!n||n>MAXDWORD) error=L"Enter a running process ID.";
                else {
                    HANDLE process=OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,FALSE,DWORD(n));
                    FILETIME created,exited,kernel,user;
                    if(!process||!GetProcessTimes(process,&created,&exited,&kernel,&user))
                        error=L"The process creation time could not be read. Choose a running process.";
                    else {
                        ULARGE_INTEGER time; time.LowPart=created.dwLowDateTime; time.HighPart=created.dwHighDateTime;
                        targets="[{\"kind\":\"process_instance\",\"pid\":"+std::to_string(n)+
                            ",\"start_time_filetime\":\""+std::to_string(time.QuadPart)+"\",\"include_descendants\":true}]";
                    }
                    if(process) CloseHandle(process);
                }
            } else if(mode==3) {
                if(target.size()<3 || !((target[1]==L':'&&(target[2]==L'\\'||target[2]==L'/')) || target.substr(0,2)==L"\\\\"))
                    error=L"Enter the complete executable path.";
                else targets="[{\"kind\":\"exe_path\",\"path\":"+debugJsonString(target)+"}]";
            }
            std::wstring duration=edit(window,505); unsigned long long minutes=0;
            if(!duration.empty() && (!integer(duration,minutes)||!minutes||minutes>~0ull/60000))
                error=L"Enter a positive number of minutes.";
            if(!error.empty()) { MessageBoxW(window,error.c_str(),L"Debug filter",MB_OK|MB_ICONWARNING); return TRUE; }
            form->json="{\"filter\":{\"components\":"+list(edit(window,501))+",\"levels\":"+list(edit(window,502))+
                       ",\"targets\":"+targets+"}";
            if(minutes) form->json+=",\"duration_ms\":"+std::to_string(minutes*60000);
            form->json+="}"; EndDialog(window,1); return TRUE;
        }
    }
    return FALSE;
}

class DebugPage : public Page {
public:
    explicit DebugPage(HWND window):window_(window) { SetTimer(window_,kRefresh,2500,nullptr); launch(); }
    ~DebugPage() override { KillTimer(window_,kRefresh); if(worker_.joinable()) worker_.join(); }
    const wchar_t* title() const override { return L"Debug channel"; }
    const wchar_t* icon() const override { return L""; }
    bool animating() const override { return busy_; }
    void draw(Ui& ui,float x,float w,float& y) override {
        auto row=[&](const std::wstring& title,const std::wstring& desc) {
            return ui.settingsCard(x,y,w,L"",title,desc);
        };
        float cy=row(L"Send debug output to host",status_.valid?(status_.forwarding?L"On":L"Off"):L"Checking debug service…");
        ui.toggle(x+w-20,cy,Forward,status_.forwarding,status_.valid&&!busy_);
        if(!status_.error.empty())
            ui.infoBar(x,w,y,Ui::Severity::kError,L"Debug service",status_.error);
        if(status_.valid) {
            row(L"Serial driver",status_.driver);
            row(L"Controller",status_.controller);
            row(L"Debug port",status_.port);
            row(L"Host connection",status_.host);
            row(L"Current filter · Revision "+status_.revision,status_.filter);
            if(!status_.expires.empty()) row(L"Filter expires",status_.expires);
        }
        if(!busy_) {
            ui.button(L"Refresh",x,y,Refresh,false);
            if(status_.valid) {
                ui.button(L"Edit filter",x+110,y,Filter,true);
                ui.button(L"Reset filter",x+235,y,Reset,false);
            }
            if(driverMissing_) ui.button(L"Install serial driver",x,y+44,Install,true);
        } else ui.progressRing(x+16,y+16,24,-1);
        y+=92;
    }
    void click(int id) override {
        if(busy_) return;
        if(id==Refresh) launch();
        if(id==Forward||id==Filter||id==Reset) {
            if(!elevated()) {
                if(!restartElevated(window_,6)) status_.error=L"Administrator approval was not completed.";
                return;
            }
            if(id==Forward) launch({L"forwarding",status_.forwarding?L"off":L"on"});
            if(id==Reset) launch({L"filter-reset"});
            if(id==Filter) {
                FilterForm form;
                form.current=status_.effectiveFilter;
                form.summary=status_.filter;
                if(DialogBoxParamW(GetModuleHandleW(nullptr),MAKEINTRESOURCEW(500),window_,filterProc,reinterpret_cast<LPARAM>(&form))==1)
                    launch({},form.json);
            }
        }
        if(id==Install) {
            wchar_t folder[MAX_PATH];
            if(FAILED(SHGetFolderPathW(nullptr,CSIDL_PROGRAM_FILES,nullptr,SHGFP_TYPE_CURRENT,folder))) {
                status_.error=L"The application installation folder could not be located."; return;
            }
            std::wstring setup=std::wstring(folder)+L"\\Venera\\venera-setup.exe";
            SHELLEXECUTEINFOW info={sizeof(info)}; info.lpVerb=L"runas"; info.lpFile=setup.c_str();
            info.lpParameters=L"/vioserial"; info.hwnd=window_; info.nShow=SW_SHOWNORMAL;
            if(!ShellExecuteExW(&info)) status_.error=L"Serial driver setup could not start ("+std::to_wstring(GetLastError())+L").";
        }
    }
    bool message(UINT msg,WPARAM wp,LPARAM) override {
        if(msg==WM_TIMER&&wp==kRefresh) { if(!busy_) launch(); return true; }
        if(msg!=kResult) return false;
        if(worker_.joinable()) worker_.join();
        { std::lock_guard<std::mutex> lock(mutex_); status_=std::move(pending_); }
        busy_=false;
        driverMissing_=status_.driver==L"missing";
        return true;
    }
private:
    void launch(std::vector<std::wstring> args={L"status",L"--json"},std::string filter={}) {
        if(worker_.joinable()) worker_.join(); busy_=true;
        worker_=std::thread([this,args=std::move(args),filter=std::move(filter)]() mutable {
            DebugStatus result; std::wstring file;
            if(!filter.empty()) {
                wchar_t temp[MAX_PATH],name[MAX_PATH];
                if(GetTempPathW(MAX_PATH,temp)&&GetTempFileNameW(temp,L"vdf",0,name)) {
                    file=name; HANDLE f=CreateFileW(file.c_str(),GENERIC_WRITE,0,nullptr,TRUNCATE_EXISTING,FILE_ATTRIBUTE_TEMPORARY,nullptr);
                    DWORD written=0;
                    if(f==INVALID_HANDLE_VALUE||!WriteFile(f,filter.data(),DWORD(filter.size()),&written,nullptr)||written!=filter.size())
                        result.error=L"The filter could not be saved.";
                    if(f!=INVALID_HANDLE_VALUE) CloseHandle(f);
                    args={L"filter-set",L"--json",file};
                } else result.error=L"The filter file could not be created.";
            }
            if(result.error.empty()) result=queryDebug(args);
            if(!result.valid) {
                SC_HANDLE manager=OpenSCManagerW(nullptr,nullptr,SC_MANAGER_CONNECT);
                if(manager) {
                    SC_HANDLE driver=OpenServiceW(manager,L"VirtioSerial",SERVICE_QUERY_STATUS);
                    if(driver) CloseServiceHandle(driver);
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
    bool busy_=false,driverMissing_=false;
    DebugStatus status_,pending_;
    std::thread worker_;
    std::mutex mutex_;
};
}
std::unique_ptr<Page> makeDebug(HWND window) { return std::make_unique<DebugPage>(window); }
}
