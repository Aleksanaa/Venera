// SPDX-License-Identifier: MIT
#pragma once
#include <initializer_list>
#include <string>
#include <vector>

namespace panel::locale {
enum class Language { English, Chinese };
enum class Preference { System, English, Chinese };
Language select(Preference preference, const std::vector<std::wstring>& preferred);
Preference parsePreference(const std::wstring& value);
const wchar_t* preferenceName(Preference preference);
std::wstring render(const std::wstring& message, const std::vector<std::wstring>& args);
}
