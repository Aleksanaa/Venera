// SPDX-License-Identifier: MIT
#pragma once
#include "locale_core.h"
#include "strings.h"

namespace panel::locale {
const wchar_t* name();
Language language();
Preference preference();
void init();
bool change(Preference preference);
const wchar_t* text(unsigned id);
const wchar_t* text(unsigned id, Language language);
std::wstring format(unsigned id, std::initializer_list<std::wstring> args);
unsigned short resourceLanguage();
struct Message {
    unsigned id = 0;
    std::vector<std::wstring> args;
    std::wstring diagnostic;
    Message() = default;
    Message(int message, std::initializer_list<std::wstring> values = {}) : id(message), args(values) {}
    Message(std::wstring raw) : diagnostic(std::move(raw)) {}
    Message(const wchar_t* raw) : diagnostic(raw) {}
    bool empty() const { return !id && diagnostic.empty(); }
    std::wstring resolve() const { return (id ? render(text(id), args) : std::wstring()) + diagnostic; }
    std::wstring resolve(Language language) const { return (id ? render(text(id, language), args) : std::wstring()) + diagnostic; }
    operator std::wstring() const { return resolve(); }
};
}
