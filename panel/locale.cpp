// SPDX-License-Identifier: MIT
#include "locale.h"
#include <windows.h>
#include <array>
#include <atomic>
#include <map>
#include <mutex>
#include <stdexcept>

namespace panel::locale {
namespace {
constexpr wchar_t kKey[] = L"SOFTWARE\\Venera\\Panel";
Preference chosen = Preference::System;
std::atomic<Language> current{Language::English};
std::array<std::map<unsigned, std::wstring>, 2> catalog;
std::mutex mutex;

std::vector<std::wstring> preferred()
{
    ULONG count = 0, size = 0;
    GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &count, nullptr, &size);
    std::vector<wchar_t> buffer(size);
    std::vector<std::wstring> result;
    if (size && GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &count, buffer.data(), &size))
        for (const wchar_t* p = buffer.data(); *p; p += wcslen(p) + 1)
            result.emplace_back(p);
    return result;
}

std::wstring load(unsigned id, WORD language)
{
    HMODULE module = GetModuleHandleW(nullptr);
    HRSRC resource = FindResourceExW(module, RT_STRING, MAKEINTRESOURCEW(id / 16 + 1), language);
    if (!resource)
        return {};
    auto* p = static_cast<const wchar_t*>(LockResource(LoadResource(module, resource)));
    if (!p)
        return {};
    for (unsigned i = 0; i < id % 16; ++i)
        p += *p + 1;
    return std::wstring(p + 1, *p);
}
}

void init()
{
    chosen = Preference::System;
    wchar_t value[32] = {};
    DWORD bytes = sizeof(value);
    if (RegGetValueW(HKEY_CURRENT_USER, kKey, L"Language", RRF_RT_REG_SZ, nullptr, value, &bytes) == ERROR_SUCCESS) {
        chosen = parsePreference(value);
    }
    current = select(chosen, preferred());
}

bool change(Preference preference)
{
    const wchar_t* value = preferenceName(preference);
    if (RegSetKeyValueW(HKEY_CURRENT_USER, kKey, L"Language", REG_SZ, value, DWORD((wcslen(value) + 1) * sizeof(wchar_t))) != ERROR_SUCCESS)
        return false;
    chosen = preference;
    current = select(chosen, preferred());
    return true;
}

Language language() { return current.load(); }
Preference preference() { return chosen; }
const wchar_t* name() { return current == Language::Chinese ? L"zh-CN" : L"en-US"; }
unsigned short resourceLanguage() { return current == Language::Chinese ? 0x0804 : 0x0409; }

const wchar_t* text(unsigned id)
{
    return text(id, language());
}

const wchar_t* text(unsigned id, Language selected)
{
    std::lock_guard<std::mutex> lock(mutex);
    auto& strings = catalog[selected == Language::Chinese ? 1 : 0];
    auto it = strings.find(id);
    if (it == strings.end()) {
        auto value = load(id, selected == Language::Chinese ? 0x0804 : 0x0409);
        if (value.empty()) value = load(id, 0x0409);
        it = strings.emplace(id, std::move(value)).first;
    }
    return it->second.c_str();
}

std::wstring format(unsigned id, std::initializer_list<std::wstring> args)
{
    return render(text(id), args);
}
}
