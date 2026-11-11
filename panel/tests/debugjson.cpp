// SPDX-License-Identifier: MIT
#include "../debugjson.h"
#include <cassert>

int main() {
    using namespace panel::debugjson;
    Value status;
    assert(Parser(R"({"revision":18446744073709551615,"forwarding_enabled":true,"health":{"driver":"installed"},"text":"C:\\Games\\demo.exe\n\"ready\" \uD83D\uDE80"})").parse(status));
    assert(status["revision"].text=="18446744073709551615");
    assert(status["forwarding_enabled"].boolean==true);
    assert(status["health"]["driver"].text=="installed");
    assert(status["text"].text=="C:\\Games\\demo.exe\n\"ready\" \xf0\x9f\x9a\x80");
    Value filter;
    assert(Parser(R"({"filter":{"components":["dxvk"],"levels":[],"targets":[{"kind":"exe_path","path":"C:\\A B\\a.exe"}]},"duration_ms":60000})").parse(filter));
    assert(filter["filter"]["targets"].items[0]["path"].text=="C:\\A B\\a.exe");
    Value current;
    assert(Parser(R"({"components":["dxvk","venus"],"levels":["warning","fatal"],"targets":[{"kind":"process_instance","pid":4120,"start_time_filetime":"134124000000000000","include_descendants":false},{"kind":"exe_path","path":"C:\\Games\\demo \"test\".exe"}]})").parse(current));
    assert(encode(current["components"])==R"(["dxvk","venus"])");
    assert(encode(current["levels"])==R"(["warning","fatal"])");
    assert(encode(current["targets"])==R"([{"kind":"process_instance","pid":4120,"start_time_filetime":"134124000000000000","include_descendants":false},{"kind":"exe_path","path":"C:\\Games\\demo \"test\".exe"}])");
    assert(quote("line\n\t\x01\\\"")==R"("line\u000a\u0009\u0001\\\"")");
    for(const char* input:{R"({"a":"\q"})",R"({"a":"\uD800"})",R"({"a":01})",R"({"a":1,"a":2})",R"([1,])",R"({"a":true}extra)"}) {
        Value invalid;
        assert(Parser(input).parse(invalid)==false);
    }
}
