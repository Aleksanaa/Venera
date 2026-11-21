// SPDX-License-Identifier: MIT
#include "../components.h"

#include <cstdlib>
#include <iostream>

void check(bool ok, const char* name)
{
    if (!ok) {
        std::cerr << name << '\n';
        std::exit(1);
    }
}

int main()
{
    using namespace components;
    Selection parsed;
    check(parse_selection("graphics,panel,collector,transport", parsed) &&
          parsed == Selection{true, true, true, true}, "command selection");
    check(parse_selection("graphics", parsed) && parsed == Selection{true, false, false, false}, "graphics command");
    check(!parse_selection("graphics,", parsed) && !parse_selection("graphics,panel,panel", parsed) &&
          !parse_selection("panel", parsed) && !parse_selection("graphics,unknown", parsed), "command validation");
    Observed clean;
    clean.transport_available = true;
    auto selected = current_selection(clean);
    check(selected == Selection{true, true, true, true}, "clean selection");
    auto p = plan(clean, Action::Install, selected);
    check(p.error.empty() && p.graphics && p.panel_install && p.collector_install && p.transport_install,
          "clean installation");

    Observed installed{true, true, true, true, true, true, true, {true, true, true, false}};
    selected = current_selection(installed);
    check(selected == Selection{true, true, true, false}, "upgrade selection");
    p = plan(installed, Action::Update, selected);
    check(p.graphics && p.panel_install && p.collector_install && !p.transport_install, "upgrade servicing");
    p = plan(installed, Action::Modify, {true, true, false, false});
    check(!p.graphics && !p.panel_install && p.collector_remove && !p.transport_install, "modify difference");
    installed.panel = false;
    p = plan(installed, Action::Repair, {true, true, true, false});
    check(p.graphics && p.panel_install && p.collector_install && !p.collector_remove, "repair selection");

    clean.transport_available = false;
    p = plan(clean, Action::Install, {true, true, true, false});
    check(p.error == "Select VirtIO socket transport for Debug collector.", "collector dependency");
    p = plan(clean, Action::Install, {true, true, false, true});
    check(p.error == "The VirtIO socket package is unavailable.", "transport availability");
    check(current_selection(clean) == Selection{true, true, false, false}, "clean graphics and panel");
    installed.selection_recorded = false;
    check(current_selection(installed) == Selection{true, true, true, false}, "legacy observed selection");
    installed.collector_coupled = true;
    p = plan(installed, Action::Modify, {true, false, false, false});
    check(p.graphics && p.collector_remove, "legacy collector servicing");
    installed.transport_ready = false;
    check(current_selection(installed) == Selection{true, true, true, true}, "legacy transport recovery");

    for (auto action : {Action::Install, Action::Modify, Action::Repair, Action::Update}) {
        p = plan(installed, action, {true, false, true, true});
        check(p.error == "Select Panel for Debug collector.", "collector panel dependency");
    }

    check(completed({true, true, false, false}, {true, false, true, true}, true, false, true) ==
                    Selection{true, false, false, true}, "partial completion selection");
    check(completed({true, true, false, false}, {true, false, true, true}, true, true, true) ==
                    Selection{true, false, true, true}, "completed selection");
    p = plan(clean, Action::Modify, {false, true, false, false});
    check(p.error == "Select Graphics to install Venera.", "graphics selection");
}
