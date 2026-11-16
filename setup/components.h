// SPDX-License-Identifier: MIT
#pragma once

#include <string>

namespace components {

struct Selection {
    bool graphics = true;
    bool panel = false;
    bool collector = false;
    bool transport = false;
    bool operator==(const Selection&) const = default;
};

struct Observed {
    bool installed = false;
    bool graphics = false;
    bool panel = false;
    bool collector = false;
    bool transport_ready = false;
    bool transport_available = false;
    bool selection_recorded = false;
    Selection requested;
    bool collector_coupled = false;
};

enum class Action { Install, Update, Modify, Repair };

inline bool parse_selection(const std::string& text, Selection& selection)
{
    Selection result{false, false, false, false};
    size_t begin = 0;
    while (begin < text.size()) {
        const auto end = text.find(',', begin);
        const auto name = text.substr(begin, end == std::string::npos ? end : end - begin);
        bool* field = name == "graphics" ? &result.graphics : name == "panel" ? &result.panel :
                      name == "collector" ? &result.collector : name == "transport" ? &result.transport : nullptr;
        if (!field || *field)
            return false;
        *field = true;
        if (end == std::string::npos) {
            if (!result.graphics)
                return false;
            selection = result;
            return true;
        }
        begin = end + 1;
    }
    return false;
}

inline Selection current_selection(const Observed& state)
{
    if (state.selection_recorded)
        return state.requested;
    if (state.installed)
        return {true, state.panel || state.collector, state.collector, state.collector && !state.transport_ready};
    return {true, true, state.transport_available || state.transport_ready, state.transport_available};
}

struct Plan {
    Selection selected;
    bool graphics = false;
    bool panel_install = false;
    bool panel_remove = false;
    bool collector_install = false;
    bool collector_remove = false;
    bool transport_install = false;
    std::string error;
};

inline Plan plan(const Observed& state, Action action, Selection selected)
{
    Plan p;
    p.selected = selected;
    if (!selected.graphics)
        p.error = "Select Graphics to install Venera.";
    else if (selected.collector && !selected.panel)
        p.error = "Select Panel for Debug collector.";
    else if (selected.transport && !state.transport_available && !state.transport_ready)
        p.error = "The VirtIO socket package is unavailable.";
    else if (selected.collector && !selected.transport && !state.transport_ready)
        p.error = "Select VirtIO socket transport for Debug collector.";
    if (!p.error.empty())
        return p;
    p.graphics = action != Action::Modify || !state.graphics || state.collector_coupled;
    p.transport_install = selected.transport && state.transport_available &&
                          (action != Action::Modify || !state.transport_ready);
    p.panel_install = selected.panel && (action != Action::Modify || !state.panel);
    p.panel_remove = !selected.panel && state.panel;
    p.collector_install = selected.collector && (p.graphics || action != Action::Modify || !state.collector);
    p.collector_remove = !selected.collector && state.collector;
    return p;
}

// A completed component records its requested state after servicing succeeds.
inline Selection completed(Selection recorded, Selection requested, bool panel, bool collector, bool transport)
{
    if (panel)
        recorded.panel = requested.panel;
    if (collector)
        recorded.collector = requested.collector;
    if (transport)
        recorded.transport = requested.transport;
    return recorded;
}

} // namespace components
