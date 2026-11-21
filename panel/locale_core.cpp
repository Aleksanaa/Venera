// SPDX-License-Identifier: MIT
#include "locale_core.h"
#include <stdexcept>

namespace panel::locale {
Language select(Preference preference, const std::vector<std::wstring>& preferred)
{
    if (preference == Preference::English) return Language::English;
    if (preference == Preference::Chinese) return Language::Chinese;
    for (auto name : preferred) {
        for (auto& c : name) if (c >= L'A' && c <= L'Z') c += L'a' - L'A';
        if (name == L"zh-cn" || name == L"zh-sg" || name == L"zh-hans" || name.rfind(L"zh-hans-", 0) == 0)
            return Language::Chinese;
        if (name == L"en" || name.rfind(L"en-", 0) == 0)
            return Language::English;
    }
    return Language::English;
}

Preference parsePreference(const std::wstring& value)
{
    if (value == L"en-US") return Preference::English;
    if (value == L"zh-CN") return Preference::Chinese;
    return Preference::System;
}

const wchar_t* preferenceName(Preference preference)
{
    return preference == Preference::English ? L"en-US" : preference == Preference::Chinese ? L"zh-CN" : L"system";
}

std::wstring render(const std::wstring& message, const std::vector<std::wstring>& args)
{
    std::wstring result;
    std::vector<bool> used(args.size());
    for (size_t i = 0; i < message.size();) {
        if (message[i] == L'{') {
            if (i + 2 >= message.size() || message[i + 2] != L'}' || message[i + 1] < L'0' || message[i + 1] > L'9')
                throw std::invalid_argument("message placeholder");
            size_t index = message[i + 1] - L'0';
            if (index >= args.size()) throw std::invalid_argument("message argument");
            result += *(args.begin() + index);
            used[index] = true;
            i += 3;
        } else if (message[i] == L'}') {
            throw std::invalid_argument("message placeholder");
        } else result += message[i++];
    }
    for (bool present : used) if (!present) throw std::invalid_argument("unused message argument");
    return result;
}
}
