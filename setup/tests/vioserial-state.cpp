// SPDX-License-Identifier: MIT
#include "../vioserial-state.h"
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
    using vioserial::accepted;
    using vioserial::ready;
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
    expect(vioserial::matches(L"PCI\\VEN_1AF4&DEV_1003&SUBSYS_00031AF4&REV_00"), true, "legacy hardware ID");
    expect(vioserial::matches(L"pci\\ven_1af4&dev_1043"), true, "modern compatible ID");
    expect(vioserial::matches(L"PCI\\VEN_1AF4&DEV_10430"), false, "device ID token");
    expect(vioserial::matches(L"PCI\\VEN_1234&DEV_1043"), false, "vendor ID");
    std::cout << "17 literal decisions passed\n";
}
