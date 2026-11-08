// SPDX-License-Identifier: MIT
// Updates from GitHub, for the About page and venera-panel /update. Two channels: Release, the newest tagged release
// (its venera-setup-<version>.exe), and Develop, the newest build of main from the build workflow (an artifact, a zip
// with the installer and a disc image). The version and GitHub's SHA-256 of the file come from GitHub's API alone.
// GitHub lets only signed-in users download artifacts, so builds of main come through nightly.link, which passes them
// on from GitHub's storage; the checksum makes it the same file.
//
// The window downloads without administrator rights, to show the progress; the install runs as administrator
// (/update CHANNEL /from FILE) and trusts nothing it is given: it asks GitHub again, copies the file into a new
// directory only SYSTEM and Administrators can open, checking the checksum of what it writes (it downloads the file
// itself when that does not match), unpacks the zip there with Windows' tar.exe, checks the installer's version
// resource and starts it.
#include "panel.h"

#include <bcrypt.h>
#include <sddl.h>
#include <winhttp.h>

#include <cstdlib>

namespace panel {

namespace {

constexpr wchar_t kApi[] = L"https://api.github.com/repos/Aleksanaa/Venera";
constexpr wchar_t kArtifacts[] = L"https://nightly.link/Aleksanaa/Venera/actions/artifacts/";
constexpr wchar_t kWorkflow[] = L"build.yml";
constexpr wchar_t kSettingsKey[] = L"Software\\Venera\\Panel";

// ---- JSON: just enough to read GitHub's answers -------------------------------------------------------------------

struct Json {
    enum Type { kNull, kBool, kNumber, kString, kArray, kObject } type = kNull;
    std::string s;  // a string, or a number as written (run and artifact ids are past a double's precision)
    bool b = false;
    std::vector<Json> items;
    std::vector<std::pair<std::string, Json>> members;

    const Json& operator[](const char* key) const
    {
        static const Json null;
        for (const auto& m : members)
            if (m.first == key)
                return m.second;
        return null;
    }
    std::wstring str() const { return type == kString || type == kNumber ? widen(s) : std::wstring(); }
};

class JsonParser {
public:
    explicit JsonParser(const std::string& text) : p_(text.data()), end_(text.data() + text.size()) {}
    bool parse(Json* out)
    {
        bool ok = value(out, 0);
        space();
        return ok && p_ == end_;
    }

private:
    void space()
    {
        while (p_ < end_ && (*p_ == ' ' || *p_ == '\t' || *p_ == '\n' || *p_ == '\r'))
            p_++;
    }
    bool literal(const char* word)
    {
        size_t n = strlen(word);
        if (size_t(end_ - p_) < n || memcmp(p_, word, n))
            return false;
        p_ += n;
        return true;
    }
    static void utf8(std::string& out, unsigned c)
    {
        if (c < 0x80) {
            out += char(c);
        } else if (c < 0x800) {
            out += char(0xc0 | c >> 6);
            out += char(0x80 | (c & 0x3f));
        } else if (c < 0x10000) {
            out += char(0xe0 | c >> 12);
            out += char(0x80 | (c >> 6 & 0x3f));
            out += char(0x80 | (c & 0x3f));
        } else {
            out += char(0xf0 | c >> 18);
            out += char(0x80 | (c >> 12 & 0x3f));
            out += char(0x80 | (c >> 6 & 0x3f));
            out += char(0x80 | (c & 0x3f));
        }
    }
    bool hex4(unsigned* c)
    {
        if (end_ - p_ < 4)
            return false;
        char buf[5] = {p_[0], p_[1], p_[2], p_[3], 0};
        char* e;
        *c = strtoul(buf, &e, 16);
        p_ += 4;
        return e == buf + 4;
    }
    bool string(std::string* out)
    {
        p_++;  // "
        while (p_ < end_ && *p_ != '"') {
            if (*p_ != '\\') {
                *out += *p_++;
                continue;
            }
            if (++p_ == end_)
                return false;
            char e = *p_++;
            switch (e) {
            case 'b': *out += '\b'; break;
            case 'f': *out += '\f'; break;
            case 'n': *out += '\n'; break;
            case 'r': *out += '\r'; break;
            case 't': *out += '\t'; break;
            case 'u': {
                unsigned c, low;
                if (!hex4(&c))
                    return false;
                if (c >= 0xd800 && c < 0xdc00 && end_ - p_ >= 6 && p_[0] == '\\' && p_[1] == 'u') {
                    p_ += 2;
                    if (!hex4(&low))
                        return false;
                    c = 0x10000 + ((c - 0xd800) << 10) + (low - 0xdc00);
                }
                utf8(*out, c);
                break;
            }
            default: *out += e;
            }
        }
        if (p_ == end_)
            return false;
        p_++;
        return true;
    }
    bool value(Json* v, int depth)
    {
        space();
        if (p_ == end_ || depth > 64)
            return false;
        char c = *p_;
        if (c == '{') {
            v->type = Json::kObject;
            p_++;
            space();
            if (p_ < end_ && *p_ == '}')
                return ++p_, true;
            for (;;) {
                space();
                std::pair<std::string, Json> m;
                if (p_ == end_ || *p_ != '"' || !string(&m.first))
                    return false;
                space();
                if (p_ == end_ || *p_++ != ':' || !value(&m.second, depth + 1))
                    return false;
                v->members.push_back(std::move(m));
                space();
                if (p_ < end_ && *p_ == ',') {
                    p_++;
                    continue;
                }
                return p_ < end_ && *p_++ == '}';
            }
        }
        if (c == '[') {
            v->type = Json::kArray;
            p_++;
            space();
            if (p_ < end_ && *p_ == ']')
                return ++p_, true;
            for (;;) {
                v->items.emplace_back();
                if (!value(&v->items.back(), depth + 1))
                    return false;
                space();
                if (p_ < end_ && *p_ == ',') {
                    p_++;
                    continue;
                }
                return p_ < end_ && *p_++ == ']';
            }
        }
        if (c == '"') {
            v->type = Json::kString;
            return string(&v->s);
        }
        if (literal("true")) {
            v->type = Json::kBool;
            v->b = true;
            return true;
        }
        if (literal("false")) {
            v->type = Json::kBool;
            return true;
        }
        if (literal("null"))
            return true;
        const char* start = p_;
        while (p_ < end_ && (isdigit(static_cast<unsigned char>(*p_)) || strchr("+-.eE", *p_)))
            p_++;
        v->type = Json::kNumber;
        v->s.assign(start, p_);
        return p_ > start;
    }

    const char* p_;
    const char* end_;
};

// ---- HTTPS ----------------------------------------------------------------------------------------------------------

std::wstring systemMessage(DWORD error)
{
    wchar_t* text = nullptr;
    DWORD flags = FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_IGNORE_INSERTS;
    HMODULE winhttp = GetModuleHandleW(L"winhttp.dll");
    if (error >= WINHTTP_ERROR_BASE && error <= WINHTTP_ERROR_LAST && winhttp)
        flags |= FORMAT_MESSAGE_FROM_HMODULE;
    else
        flags |= FORMAT_MESSAGE_FROM_SYSTEM;
    FormatMessageW(flags, winhttp, error, 0, reinterpret_cast<LPWSTR>(&text), 0, nullptr);
    std::wstring s = text ? text : L"error " + std::to_wstring(error);
    LocalFree(text);
    while (!s.empty() && (s.back() == L'\n' || s.back() == L'\r' || s.back() == L'.'))
        s.pop_back();
    return s;
}

struct Handle {
    HINTERNET h = nullptr;
    ~Handle()
    {
        if (h)
            WinHttpCloseHandle(h);
    }
};

// GETs an https URL, following redirects to https only: GitHub's JSON, or else a file (from GitHub's API, a release's
// asset). `sink` takes the body as it comes, with its whole length (0 when the server does not say), and returns false
// to stop. Returns why it failed, else empty.
std::wstring get(const std::wstring& url, bool json,
                 const std::function<bool(const char* data, DWORD size, ULONGLONG total)>& sink)
{
    URL_COMPONENTS u = {sizeof(u)};
    u.dwHostNameLength = u.dwUrlPathLength = u.dwExtraInfoLength = DWORD(-1);
    if (!WinHttpCrackUrl(url.c_str(), 0, 0, &u) || u.nScheme != INTERNET_SCHEME_HTTPS)
        return L"Not an https address: " + url;
    std::wstring host(u.lpszHostName, u.dwHostNameLength);
    std::wstring path = std::wstring(u.lpszUrlPath, u.dwUrlPathLength) + std::wstring(u.lpszExtraInfo, u.dwExtraInfoLength);

    Handle session, connection, request;
    session.h = WinHttpOpen(L"venera-panel", WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_NO_PROXY_NAME,
                            WINHTTP_NO_PROXY_BYPASS, 0);
    if (!session.h)
        return L"Connecting failed: " + systemMessage(GetLastError());
    WinHttpSetTimeouts(session.h, 15000, 15000, 30000, 30000);
    connection.h = WinHttpConnect(session.h, host.c_str(), u.nPort, 0);
    if (connection.h)
        request.h = WinHttpOpenRequest(connection.h, L"GET", path.c_str(), nullptr, WINHTTP_NO_REFERER,
                                       WINHTTP_DEFAULT_ACCEPT_TYPES, WINHTTP_FLAG_SECURE);
    bool api = host == L"api.github.com";
    std::wstring headers = json ? L"Accept: application/vnd.github+json\r\n" : L"Accept: application/octet-stream\r\n";
    if (api)
        headers += L"X-GitHub-Api-Version: 2022-11-28\r\n";
    if (!request.h || !WinHttpSendRequest(request.h, headers.c_str(), DWORD(-1), WINHTTP_NO_REQUEST_DATA, 0, 0, 0) ||
        !WinHttpReceiveResponse(request.h, nullptr))
        return L"Connecting to " + host + L" failed: " + systemMessage(GetLastError());

    DWORD status = 0, length = 0, size = sizeof(status);
    WinHttpQueryHeaders(request.h, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_HEADER_NAME_BY_INDEX,
                        &status, &size, WINHTTP_NO_HEADER_INDEX);
    if (status != 200) {
        if (api && (status == 403 || status == 429))
            return L"GitHub turned the request down (HTTP " + std::to_wstring(status) +
                   L"): it takes 60 an hour from an address without signing in. Try again later";
        return host + L" answered HTTP " + std::to_wstring(status);
    }
    size = sizeof(length);
    WinHttpQueryHeaders(request.h, WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
                        WINHTTP_HEADER_NAME_BY_INDEX, &length, &size, WINHTTP_NO_HEADER_INDEX);
    std::vector<char> buf(256 * 1024);
    for (;;) {
        DWORD got = 0;
        if (!WinHttpReadData(request.h, buf.data(), DWORD(buf.size()), &got))
            return L"Downloading from " + host + L" failed: " + systemMessage(GetLastError());
        if (!got)
            return {};
        if (!sink(buf.data(), got, length))
            return L"Cancelled";
    }
}

std::wstring getJson(const std::wstring& url, Json* out)
{
    std::string body;
    std::wstring e = get(url, true, [&](const char* d, DWORD n, ULONGLONG) {
        body.append(d, n);
        return body.size() < (64u << 20);
    });
    if (!e.empty())
        return e;
    if (!JsonParser(body).parse(out))
        return L"GitHub's answer is not JSON";
    return {};
}

// ---- SHA-256 ------------------------------------------------------------------------------------------------------

class Sha256 {
public:
    Sha256()
    {
        BCryptOpenAlgorithmProvider(&alg_, BCRYPT_SHA256_ALGORITHM, nullptr, 0);
        if (alg_)
            BCryptCreateHash(alg_, &hash_, nullptr, 0, nullptr, 0, 0);
    }
    ~Sha256()
    {
        if (hash_)
            BCryptDestroyHash(hash_);
        if (alg_)
            BCryptCloseAlgorithmProvider(alg_, 0);
    }
    void add(const void* data, DWORD size)
    {
        if (hash_)
            BCryptHashData(hash_, static_cast<PUCHAR>(const_cast<void*>(data)), size, 0);
    }
    std::string hex()
    {
        UCHAR digest[32];
        if (!hash_ || BCryptFinishHash(hash_, digest, sizeof(digest), 0))
            return {};
        std::string s;
        for (UCHAR b : digest) {
            s += "0123456789abcdef"[b >> 4];
            s += "0123456789abcdef"[b & 15];
        }
        return s;
    }

private:
    BCRYPT_ALG_HANDLE alg_ = nullptr;
    BCRYPT_HASH_HANDLE hash_ = nullptr;
};

// GitHub writes checksums as "sha256:<hex>".
std::string sha256Of(const Json& digest)
{
    const std::string& s = digest.s;
    if (s.compare(0, 7, "sha256:") || s.size() != 7 + 64)
        return {};
    std::string hex = s.substr(7);
    for (char& c : hex)
        c = char(tolower(static_cast<unsigned char>(c)));
    return hex;
}

// ---- files --------------------------------------------------------------------------------------------------------

struct File {
    HANDLE h = INVALID_HANDLE_VALUE;
    ~File()
    {
        if (h != INVALID_HANDLE_VALUE)
            CloseHandle(h);
    }
};

bool writeAll(HANDLE h, const char* data, DWORD size)
{
    DWORD written = 0;
    return WriteFile(h, data, size, &written, nullptr) && written == size;
}

// Downloads `url` into the new file `path`; *sha256 is the checksum of what it wrote.
std::wstring downloadTo(const std::wstring& url, const std::wstring& path, std::string* sha256,
                        const std::function<bool(ULONGLONG, ULONGLONG)>& progress)
{
    File f;
    f.h = CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (f.h == INVALID_HANDLE_VALUE)
        return L"Creating " + path + L" failed: " + systemMessage(GetLastError());
    Sha256 hash;
    ULONGLONG done = 0;
    bool writeFailed = false;
    std::wstring e = get(url, false, [&](const char* d, DWORD n, ULONGLONG total) {
        if (!writeAll(f.h, d, n)) {
            writeFailed = true;
            return false;
        }
        hash.add(d, n);
        done += n;
        return !progress || progress(done, total);
    });
    if (writeFailed)
        return L"Writing " + path + L" failed: " + systemMessage(GetLastError());
    if (!e.empty())
        return e;
    *sha256 = hash.hex();
    return {};
}

// Copies `from` into the new file `to`; *sha256 is the checksum of what it wrote.
bool copyTo(const std::wstring& from, const std::wstring& to, std::string* sha256)
{
    File in, out;
    in.h = CreateFileW(from.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL,
                       nullptr);
    out.h = CreateFileW(to.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (in.h == INVALID_HANDLE_VALUE || out.h == INVALID_HANDLE_VALUE)
        return false;
    Sha256 hash;
    std::vector<char> buf(1 << 20);
    for (;;) {
        DWORD got = 0;
        if (!ReadFile(in.h, buf.data(), DWORD(buf.size()), &got, nullptr))
            return false;
        if (!got)
            break;
        if (!writeAll(out.h, buf.data(), got))
            return false;
        hash.add(buf.data(), got);
    }
    *sha256 = hash.hex();
    return true;
}

std::string fileSha256(const std::wstring& path)
{
    File f;
    f.h = CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL,
                      nullptr);
    if (f.h == INVALID_HANDLE_VALUE)
        return {};
    Sha256 hash;
    std::vector<char> buf(1 << 20);
    for (DWORD got; ReadFile(f.h, buf.data(), DWORD(buf.size()), &got, nullptr) && got;)
        hash.add(buf.data(), got);
    return hash.hex();
}

std::wstring windowsDir()
{
    wchar_t dir[MAX_PATH];
    UINT n = GetSystemWindowsDirectoryW(dir, MAX_PATH);
    return n && n < MAX_PATH ? dir : L"C:\\Windows";
}

// A new directory in Windows' Temp that only SYSTEM and Administrators can open, with a random name: users can make
// directories there, but not rename or delete others' (as they could in their own %TEMP%), so what is in it stays as
// this process wrote it.
std::wstring makePrivateDir(std::wstring* error)
{
    UCHAR random[8];
    BCryptGenRandom(nullptr, random, sizeof(random), BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    std::wstring dir = windowsDir() + L"\\Temp\\venera-update-";
    for (UCHAR b : random) {
        dir += L"0123456789abcdef"[b >> 4];
        dir += L"0123456789abcdef"[b & 15];
    }
    SECURITY_ATTRIBUTES sa = {sizeof(sa)};
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(L"O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
                                                              SDDL_REVISION_1, &sa.lpSecurityDescriptor, nullptr)) {
        *error = L"Making the update's directory failed: " + systemMessage(GetLastError());
        return {};
    }
    BOOL made = CreateDirectoryW(dir.c_str(), &sa);
    DWORD e = GetLastError();
    LocalFree(sa.lpSecurityDescriptor);
    if (!made) {
        *error = L"Making " + dir + L" failed: " + systemMessage(e);
        return {};
    }
    return dir;
}

// Runs System32's tar.exe (bsdtar, which reads zips) to take `member` out of `zip` into `dir`.
std::wstring unzip(const std::wstring& zip, const std::wstring& member, const std::wstring& dir)
{
    std::wstring tar = windowsDir() + L"\\System32\\tar.exe";
    std::wstring cmd = L"\"" + tar + L"\" -x -f \"" + zip + L"\" -C \"" + dir + L"\" \"" + member + L"\"";
    STARTUPINFOW si = {sizeof(si)};
    PROCESS_INFORMATION pi;
    if (!CreateProcessW(tar.c_str(), cmd.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW, nullptr, dir.c_str(), &si,
                        &pi))
        return L"Running tar.exe failed: " + systemMessage(GetLastError());
    DWORD code = 1;
    if (WaitForSingleObject(pi.hProcess, 120000) == WAIT_OBJECT_0)
        GetExitCodeProcess(pi.hProcess, &code);
    else
        TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (code || GetFileAttributesW((dir + L"\\" + member).c_str()) == INVALID_FILE_ATTRIBUTES)
        return L"Unpacking " + member + L" from the build failed";
    return {};
}

// The FileVersion of an executable's version resource (setup/payload.cmake writes the whole version there).
std::wstring fileVersion(const std::wstring& path)
{
    DWORD ignored, size = GetFileVersionInfoSizeW(path.c_str(), &ignored);
    if (!size)
        return {};
    std::vector<BYTE> data(size);
    wchar_t* value = nullptr;
    UINT length = 0;
    if (!GetFileVersionInfoW(path.c_str(), 0, size, data.data()) ||
        !VerQueryValueW(data.data(), L"\\StringFileInfo\\040904B0\\FileVersion", reinterpret_cast<void**>(&value),
                        &length) || !length)
        return {};
    return std::wstring(value, wcsnlen(value, length));
}

bool endsWith(const std::wstring& s, const std::wstring& end)
{
    return s.size() >= end.size() && !s.compare(s.size() - end.size(), end.size(), end);
}

}  // namespace

const wchar_t* channelName(Channel c) { return c == Channel::kDevelop ? L"develop" : L"release"; }

Channel savedChannel()
{
    wchar_t buf[32];
    DWORD size = sizeof(buf);
    if (RegGetValueW(HKEY_CURRENT_USER, kSettingsKey, L"UpdateChannel", RRF_RT_REG_SZ, nullptr, buf, &size) ==
            ERROR_SUCCESS && !_wcsicmp(buf, L"develop"))
        return Channel::kDevelop;
    return Channel::kRelease;
}

void saveChannel(Channel c)
{
    const wchar_t* name = channelName(c);
    RegSetKeyValueW(HKEY_CURRENT_USER, kSettingsKey, L"UpdateChannel", REG_SZ, name,
                    DWORD((wcslen(name) + 1) * sizeof(wchar_t)));
}

int compareVersions(const std::wstring& a, const std::wstring& b)
{
    const wchar_t *p = a.c_str(), *q = b.c_str();
    for (int i = 0; i < 4; i++) {
        unsigned long x = wcstoul(p, const_cast<wchar_t**>(&p), 10), y = wcstoul(q, const_cast<wchar_t**>(&q), 10);
        if (x != y)
            return x < y ? -1 : 1;
        if (*p == L'.')
            p++;
        if (*q == L'.')
            q++;
    }
    return 0;
}

Update findUpdate(Channel c)
{
    Update u;
    Json j;
    if (c == Channel::kRelease) {
        u.error = getJson(std::wstring(kApi) + L"/releases/latest", &j);
        if (!u.error.empty())
            return u;
        for (const Json& a : j["assets"].items) {
            std::wstring name = a["name"].str();
            size_t at = name.find(L"-setup-");
            if (at == std::wstring::npos || !endsWith(name, L".exe"))
                continue;
            u.name = name;
            u.version = name.substr(at + 7, name.size() - at - 7 - 4);
            u.url = a["url"].str();  // the API's: github.com's download links fail with HTTP 504 at times
            u.sha256 = sha256Of(a["digest"]);
            u.size = _wcstoui64(a["size"].str().c_str(), nullptr, 10);
            break;
        }
        if (u.name.empty())
            u.error = L"The newest release, " + j["tag_name"].str() + L", has no installer";
    } else {
        // The newest builds of main; the newest that worked, saying so when it is not the newest of all.
        u.error = getJson(std::wstring(kApi) + L"/actions/workflows/" + kWorkflow +
                              L"/runs?branch=main&event=push&exclude_pull_requests=true&per_page=20",
                          &j);
        if (!u.error.empty())
            return u;
        const Json* run = nullptr;
        for (const Json& r : j["workflow_runs"].items)
            if (r["conclusion"].s == "success") {
                run = &r;
                break;
            }
        if (!run) {
            u.error = L"None of main's recent builds worked";
            return u;
        }
        const Json& newest = j["workflow_runs"].items.front();
        if (&newest != run) {
            std::wstring commit = newest["head_sha"].str().substr(0, 7);
            u.note = newest["status"].s != "completed" ? L"The newest commit, " + commit + L", is still building"
                                                        : L"The build of the newest commit, " + commit + L", failed";
        }
        Json artifacts;
        u.error = getJson((*run)["artifacts_url"].str(), &artifacts);
        if (!u.error.empty())
            return u;
        for (const Json& a : artifacts["artifacts"].items) {
            std::wstring name = a["name"].str();
            if (a["expired"].b || name.compare(0, 13, L"venera-setup-"))
                continue;
            u.version = name.substr(13);
            u.name = name + L".zip";
            u.url = kArtifacts + a["id"].str() + L".zip";
            u.sha256 = sha256Of(a["digest"]);
            u.size = _wcstoui64(a["size_in_bytes"].str().c_str(), nullptr, 10);
            break;
        }
        if (u.name.empty())
            u.error = L"The newest build of main that worked has no installer left (GitHub keeps them 90 days)";
    }
    // Only what can be checked. The version and the name go into paths and tar's command line.
    if (u.error.empty() && u.sha256.empty())
        u.error = L"GitHub gives no checksum for " + u.name;
    if (u.error.empty() && (u.version.empty() || u.version.find_first_not_of(L"0123456789abcdef.") != std::wstring::npos ||
                            u.name.find_first_of(L"\\/:\"*?<>|") != std::wstring::npos))
        u.error = L"Not an installer's name: " + u.name;
    return u;
}

std::wstring downloadPath(const Update& u)
{
    wchar_t temp[MAX_PATH];
    DWORD n = GetTempPathW(MAX_PATH, temp);
    std::wstring dir = std::wstring(temp, n && n < MAX_PATH ? n : 0) + L"venera-update";
    CreateDirectoryW(dir.c_str(), nullptr);
    return dir + L"\\" + u.name;
}

std::wstring downloadUpdate(const Update& u, const std::wstring& path,
                            const std::function<bool(ULONGLONG, ULONGLONG)>& progress)
{
    if (!u.sha256.empty() && fileSha256(path) == u.sha256)  // downloaded already
        return {};
    std::wstring part = path + L".part";
    std::string sha256;
    std::wstring e = downloadTo(u.url, part, &sha256, progress);
    if (e.empty() && sha256 != u.sha256)
        e = L"The download does not match GitHub's checksum";
    if (e.empty() && !MoveFileExW(part.c_str(), path.c_str(), MOVEFILE_REPLACE_EXISTING))
        e = L"Saving " + path + L" failed: " + systemMessage(GetLastError());
    if (!e.empty())
        DeleteFileW(part.c_str());
    return e;
}

std::wstring installUpdate(Channel c, const std::wstring& downloaded,
                           const std::function<void(const std::wstring&)>& say, bool* started)
{
    *started = false;
    if (!elevated())
        return L"Updating needs administrator rights";
    say(std::wstring(L"Asking GitHub for the newest ") + (c == Channel::kDevelop ? L"build of main" : L"release"));
    Update u = findUpdate(c);
    if (!u.error.empty())
        return u.error;
    std::wstring installed = installedVersion();
    if (!installed.empty() && compareVersions(u.version, installed) <= 0) {
        say(L"Venera " + installed + L" is up to date (the newest is " + u.version + L")");
        return {};
    }

    std::wstring error, dir = makePrivateDir(&error);
    if (dir.empty())
        return error;
    std::wstring file = dir + L"\\" + u.name;
    std::string sha256;
    if (downloaded.empty() || !copyTo(downloaded, file, &sha256) || sha256 != u.sha256) {
        say(L"Downloading " + u.name);
        int shown = -1;
        error = downloadTo(u.url, file, &sha256, [&](ULONGLONG done, ULONGLONG total) {
            int percent = total ? int(done * 100 / total) : -1;
            if (percent / 10 != shown / 10 && percent >= 0)
                say(std::to_wstring(shown = percent) + L" %");
            return true;
        });
        if (!error.empty())
            return error;
    }
    if (sha256 != u.sha256)
        return L"The download does not match GitHub's checksum";
    say(L"The checksum matches GitHub's");

    std::wstring exe = file;
    if (endsWith(u.name, L".zip")) {
        std::wstring member = L"venera-setup-" + u.version + L".exe";
        error = unzip(file, member, dir);
        DeleteFileW(file.c_str());
        if (!error.empty())
            return error;
        exe = dir + L"\\" + member;
    }
    if (fileVersion(exe) != u.version)
        return L"The installer's version is not " + u.version;

    say(L"Starting the installer of Venera " + u.version);
    std::wstring cmd = L"\"" + exe + L"\"";
    STARTUPINFOW si = {sizeof(si)};
    PROCESS_INFORMATION pi;
    if (!CreateProcessW(exe.c_str(), cmd.data(), nullptr, nullptr, FALSE, 0, nullptr, dir.c_str(), &si, &pi))
        return L"Starting the installer failed: " + systemMessage(GetLastError());
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    // It runs from there until it is done: gone at the next restart, which an update of the driver asks for.
    MoveFileExW(exe.c_str(), nullptr, MOVEFILE_DELAY_UNTIL_REBOOT);
    MoveFileExW(dir.c_str(), nullptr, MOVEFILE_DELAY_UNTIL_REBOOT);
    *started = true;
    return {};
}

}  // namespace panel
