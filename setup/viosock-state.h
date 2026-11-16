// SPDX-License-Identifier: MIT
#pragma once

#include <cwctype>
#include <string_view>
#include <vector>

namespace viosock {
inline bool equal_id(std::wstring_view left, std::wstring_view right)
{
    if (left.size() != right.size())
        return false;
    for (size_t i = 0; i < left.size(); ++i)
        if (std::towupper(left[i]) != std::towupper(right[i]))
            return false;
    return true;
}

inline bool matches(std::wstring_view id)
{
    for (auto base : {L"PCI\\VEN_1AF4&DEV_1012", L"PCI\\VEN_1AF4&DEV_1053"}) {
        std::wstring_view prefix(base);
        if (id.size() >= prefix.size() && equal_id(id.substr(0, prefix.size()), prefix) &&
            (id.size() == prefix.size() || id[prefix.size()] == L'&'))
            return true;
    }
    return false;
}

struct Controller {
    bool service_bound;
    bool driver_registered;
    bool status_known;
    bool started;
    unsigned long problem;
};

inline bool ready(const std::vector<Controller>& controllers, bool enumeration_complete)
{
    if (!enumeration_complete || controllers.empty())
        return false;
    for (const auto& c : controllers)
        if (!c.service_bound || !c.driver_registered || !c.status_known || !c.started || c.problem != 0)
            return false;
    return true;
}

inline bool accepted(unsigned long code, bool controllers_ready)
{
    return code == 0 || code == 3010 || code == 1641 || (code == 259 && controllers_ready);
}
struct Provider {
    bool service_running;
    bool registered;
    bool listener_ready;
};

inline bool ready(bool controllers_ready, const Provider& provider)
{
    return controllers_ready && provider.service_running && provider.registered && provider.listener_ready;
}

inline bool restart_required(unsigned long code, bool transport_ready)
{
    return code == 3010 || code == 1641 || !transport_ready;
}
} // namespace viosock
