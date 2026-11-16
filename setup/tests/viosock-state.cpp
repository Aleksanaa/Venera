// SPDX-License-Identifier: MIT
#include "../viosock-state.h"
#include <cstdlib>
#include <iostream>

int main()
{
    auto expect = [](bool actual, bool expected, const char* name) {
        if (actual != expected) {
            std::cerr << name << '\n';
            std::exit(1);
        }
    };
    using viosock::accepted;
    using viosock::ready;
    expect(accepted(259, ready({{true, true, true, true, 0}}, true)), true, "259 healthy controller");
    expect(accepted(259, ready({}, true)), false, "259 device discovery");
    expect(accepted(259, ready({{true, true, true, true, 28}}, true)), false, "259 controller problem");
    expect(accepted(259, ready({{false, true, true, true, 0}}, true)), false, "259 service binding");
    expect(accepted(259, ready({{true, false, true, true, 0}}, true)), false, "259 driver registration");
    expect(accepted(259, ready({{true, true, false, true, 0}}, true)), false, "259 status query");
    expect(accepted(259, ready({{true, true, true, false, 0}}, true)), false, "259 started state");
    expect(accepted(259, ready({{true, true, true, true, 0}, {false, true, true, true, 0}}, true)), false,
           "259 multiple controllers");
    expect(accepted(259, ready({{true, true, true, true, 0}}, false)), false, "259 enumeration result");
    expect(accepted(0, false), true, "successful installation");
    expect(accepted(3010, false), true, "restart requested");
    expect(accepted(1641, false), true, "restart initiated");
    expect(accepted(5, true), false, "installation error");
    expect(viosock::matches(L"PCI\\VEN_1AF4&DEV_1012&SUBSYS_00031AF4&REV_00"), true, "legacy hardware ID");
    expect(viosock::matches(L"pci\\ven_1af4&dev_1053"), true, "modern compatible ID");
    expect(viosock::matches(L"PCI\\VEN_1AF4&DEV_10530"), false, "device ID token");
    expect(viosock::matches(L"PCI\\VEN_1234&DEV_1053"), false, "vendor ID");
    expect(ready(true, {true, true, true}), true, "working listener");
    expect(ready(false, {true, true, true}), false, "controller activation pending");
    expect(ready(true, {false, true, true}), false, "WSP service stopped");
    expect(ready(true, {true, false, true}), false, "provider registration pending");
    expect(ready(true, {true, true, false}), false, "listener activation pending");
    expect(accepted(259, ready(true, {true, true, false})), false, "existing package activation pending");
    expect(accepted(259, ready(true, {true, true, true})), true, "existing package active");
    expect(viosock::restart_required(0, false), true, "activation after restart");
    expect(viosock::restart_required(0, true), false, "active provider");
    expect(viosock::restart_required(3010, true), true, "Windows restart request");
}
