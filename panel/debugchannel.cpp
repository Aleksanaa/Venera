// SPDX-License-Identifier: MIT
#include "debugchannel.h"
#include "debugjson.h"
#include <shellapi.h>
#include <algorithm>

namespace panel {
namespace {
using debugjson::Value;
std::wstring field(const Value& v) { return widen(v.text); }
std::wstring winError(DWORD code) {
    wchar_t* text=nullptr;
    FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER|FORMAT_MESSAGE_FROM_SYSTEM|FORMAT_MESSAGE_IGNORE_INSERTS,
                   nullptr,code,0,reinterpret_cast<LPWSTR>(&text),0,nullptr);
    std::wstring s=text?text:L"Windows error "+std::to_wstring(code); LocalFree(text); return s;
}
std::wstring guestPath() {
    std::wstring path;
    SC_HANDLE scm=OpenSCManagerW(nullptr,nullptr,SC_MANAGER_CONNECT);
    if(scm) {
        SC_HANDLE service=OpenServiceW(scm,L"VeneraDebugGuest",SERVICE_QUERY_CONFIG);
        if(service) {
            DWORD size=0; QueryServiceConfigW(service,nullptr,0,&size);
            std::vector<BYTE> data(size);
            if(size && QueryServiceConfigW(service,reinterpret_cast<QUERY_SERVICE_CONFIGW*>(data.data()),size,&size)) {
                auto* config=reinterpret_cast<QUERY_SERVICE_CONFIGW*>(data.data());
                int count=0; auto args=CommandLineToArgvW(config->lpBinaryPathName,&count);
                if(args && count) path=args[0];
                LocalFree(args);
            }
            CloseServiceHandle(service);
        }
        CloseServiceHandle(scm);
    }
    if(path.empty()) {
        wchar_t self[32768]; DWORD n=GetModuleFileNameW(nullptr,self,32768);
        path.assign(self,n); path.resize(path.find_last_of(L'\\')+1); path+=L"venera-debug-guest.exe";
    }
    return path;
}
std::wstring run(const std::vector<std::wstring>& args, DWORD timeout, HANDLE cancel,
                 const std::function<void(const std::string&)>& line) {
    std::wstring path=guestPath(), command=debugQuote(path);
    for(const auto& arg:args) command+=L" "+debugQuote(arg);
    SECURITY_ATTRIBUTES sa={sizeof(sa),nullptr,TRUE};
    HANDLE read=nullptr,write=nullptr;
    if(!CreatePipe(&read,&write,&sa,0)) return winError(GetLastError());
    SetHandleInformation(read,HANDLE_FLAG_INHERIT,0);
    HANDLE input=CreateFileW(L"NUL",GENERIC_READ,FILE_SHARE_READ|FILE_SHARE_WRITE,&sa,OPEN_EXISTING,0,nullptr);
    STARTUPINFOW si={sizeof(si)};
    si.dwFlags=STARTF_USESTDHANDLES; si.hStdOutput=write; si.hStdError=write; si.hStdInput=input;
    PROCESS_INFORMATION pi={};
    BOOL ok=CreateProcessW(path.c_str(),command.data(),nullptr,nullptr,TRUE,CREATE_NO_WINDOW,nullptr,nullptr,&si,&pi);
    DWORD error=GetLastError(); CloseHandle(write); if(input!=INVALID_HANDLE_VALUE) CloseHandle(input);
    if(!ok) { CloseHandle(read); return L"Debug service: "+winError(error); }
    CloseHandle(pi.hThread);
    ULONGLONG start=GetTickCount64(); std::string pending; std::wstring failure;
    bool exited=false;
    for(;;) {
        DWORD available=0;
        if(!PeekNamedPipe(read,nullptr,0,nullptr,&available,nullptr)) break;
        if(available) {
            char buffer[8192]; DWORD n=0;
            if(!ReadFile(read,buffer,std::min<DWORD>(available,sizeof(buffer)),&n,nullptr)) break;
            pending.append(buffer,n);
            size_t pos;
            while((pos=pending.find('\n'))!=std::string::npos) {
                std::string item=pending.substr(0,pos); pending.erase(0,pos+1);
                if(!item.empty()&&item.back()=='\r') item.pop_back();
                if(!item.empty()) line(item);
            }
            if(pending.size()>1024*1024) { failure=L"Debug message exceeds the supported size."; break; }
        } else {
            if(exited) break;
            exited=WaitForSingleObject(pi.hProcess,0)==WAIT_OBJECT_0;
            if(!exited) {
                if(cancel && WaitForSingleObject(cancel,20)==WAIT_OBJECT_0) break;
                if(!cancel) Sleep(20);
            }
        }
        if(timeout && GetTickCount64()-start>=timeout) { failure=L"Debug service request timed out."; break; }
        if(cancel && WaitForSingleObject(cancel,0)==WAIT_OBJECT_0) break;
    }
    bool cancelled=cancel && WaitForSingleObject(cancel,0)==WAIT_OBJECT_0;
    if(WaitForSingleObject(pi.hProcess,0)!=WAIT_OBJECT_0) {
        TerminateProcess(pi.hProcess,1); WaitForSingleObject(pi.hProcess,1000);
    }
    if(!pending.empty()&&!cancelled) line(pending);
    DWORD code=0; GetExitCodeProcess(pi.hProcess,&code); CloseHandle(pi.hProcess); CloseHandle(read);
    if(!failure.empty()) return failure;
    if(code&&!cancelled) return L"Debug service command failed ("+std::to_wstring(code)+L").";
    return {};
}
std::wstring state(const Value& v) {
    if(v.type==Value::String) return field(v);
    std::wstring text=field(v["state"]);
    if(v["problem"].type==Value::Number && v["problem"].text!="0") text+=L" ("+field(v["problem"])+L")";
    if(v["error"].type==Value::String) text+=L" — "+field(v["error"]);
    return text.empty()?L"Unknown":text;
}
std::wstring selection(const Value& v) {
    std::wstring s; for(const auto& item:v.items) { if(!s.empty()) s+=L", "; s+=field(item); }
    return s.empty()?L"All":s;
}
DebugStatus status(const Value& v) {
    DebugStatus s;
    if(v["forwarding_enabled"].type!=Value::Bool || v["revision"].type!=Value::Number ||
       v["effective_filter"].type!=Value::Object || v["health"].type!=Value::Object) {
        s.error=L"The debug service returned an unrecognized status."; return s;
    }
    s.valid=true; s.forwarding=v["forwarding_enabled"].boolean; s.revision=field(v["revision"]);
    s.expires=field(v["expires_at"]);
    const auto& h=v["health"]; s.driver=state(h["driver"]); s.controller=state(h["controller"]);
    s.port=state(h["port"]); s.host=state(h["host"]);
    const auto& f=v["effective_filter"];
    s.effectiveFilter=f;
    s.filter=L"Components: "+selection(f["components"])+L" · Levels: "+selection(f["levels"]);
    for(const auto& target:f["targets"].items) {
        if(target["kind"].text=="exe_path") s.filter+=L"\nProgram: "+field(target["path"]);
        else s.filter+=L"\nProcess "+field(target["pid"])+L" · Started "+field(target["start_time_filetime"])+
                          (target["include_descendants"].boolean?L" · With descendants":L"");
    }
    if(f["targets"].items.empty()) s.filter+=L" · All programs";
    return s;
}
}

std::wstring debugQuote(const std::wstring& value) {
    std::wstring out=L"\""; size_t slashes=0;
    for(wchar_t c:value) {
        if(c==L'\\') { ++slashes; continue; }
        out.append(slashes*(c==L'"'?2:1),L'\\'); slashes=0;
        if(c==L'"') out+=L'\\'; out+=c;
    }
    out.append(slashes*2,L'\\'); return out+L'"';
}
DebugStatus queryDebug(const std::vector<std::wstring>& args) {
    DebugStatus result; std::string output;
    std::wstring error=run(args,15000,nullptr,[&](const std::string& line) { if(output.size()<2*1024*1024) output+=line; });
    Value v;
    if(!debugjson::Parser(output).parse(v)) {
        result.error=error.empty()?L"The debug service returned an unrecognized response.":error;
        if(!output.empty()) result.error+=L"\n"+widen(output.substr(0,2048)); return result;
    }
    if(v["ok"].type==Value::Bool && !v["ok"].boolean) {
        result.error=field(v["error"]["message"]); if(result.error.empty()) result.error=L"The debug service could not apply the change.";
        return result;
    }
    if(!error.empty()) { result.error=error; return result; }
    if(v["status"].type==Value::Object) return status(v["status"]);
    if(v["ok"].type==Value::Bool && v["ok"].boolean) return queryDebug();
    return status(v);
}
void GuestLogs::start(std::function<void(LogLine&&)> sink) {
    stop(); { std::lock_guard<std::mutex> lock(mutex_); error_.clear(); }
    cancel_=CreateEventW(nullptr,TRUE,FALSE,nullptr); running_=true;
    thread_=std::thread([this,sink=std::move(sink)] {
        std::wstring parseError;
        auto error=run({L"logs",L"--follow"},0,cancel_,[&](const std::string& line) {
            Value v;
            if(!debugjson::Parser(line).parse(v)) { parseError=L"Debug service: "+widen(line.substr(0,2048)); return; }
            if(v["type"].text=="status") return;
            LogLine l={}; GetLocalTime(&l.time);
            if(v["type"].text=="log" && v["text"].type==Value::String) {
                l.component=field(v["component"]); l.text=field(v["text"]);
                l.level=v["level"].text=="error"?2:v["level"].text=="warn"||v["level"].text=="warning"?1:0;
                l.pid=std::strtoul(v["process"]["pid"].text.c_str(),nullptr,10);
                l.process=field(v["process"]["exe_path"]);
                if(l.process.empty()) l.process=field(v["source"]);
            } else if(v["type"].text=="gap") {
                l.component=L"service"; l.level=1; l.text=L"Log stream gap: "+field(v["reason"]);
            } else { parseError=L"The debug service returned an unrecognized log message."; return; }
            sink(std::move(l));
        });
        { std::lock_guard<std::mutex> lock(mutex_);
          error_=parseError.empty()?error:parseError;
          if(error_.empty()&&WaitForSingleObject(cancel_,0)!=WAIT_OBJECT_0) error_=L"The debug service log subscription ended.";
        }
        running_=false;
    });
}
void GuestLogs::stop() {
    if(cancel_) SetEvent(cancel_);
    if(thread_.joinable()) thread_.join();
    if(cancel_) CloseHandle(cancel_); cancel_=nullptr; running_=false;
}
}
