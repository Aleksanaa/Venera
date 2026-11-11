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
    assert(current["components"].json.dump()==R"(["dxvk","venus"])");
    assert(current["levels"].json.dump()==R"(["warning","fatal"])");
    assert(current["targets"].json.dump()==R"([{"kind":"process_instance","pid":4120,"start_time_filetime":"134124000000000000","include_descendants":false},{"kind":"exe_path","path":"C:\\Games\\demo \"test\".exe"}])");
    assert(Json("line\n\t\x01\\\"").dump()==R"("line\n\t\u0001\\\"")");
    Value primitives;
    assert(Parser(R"([null,false,true,0,-9223372036854775808,18446744073709551615,{"a":["雪",{"b":"\u0000\b\f\r"}]}])").parse(primitives));
    assert(primitives.items[0].type==Value::Null);
    assert(primitives.items[1].type==Value::Bool && primitives.items[1].boolean==false);
    assert(primitives.items[2].boolean==true);
    assert(primitives.items[4].text=="-9223372036854775808");
    assert(primitives.items[5].text=="18446744073709551615");
    assert(primitives.json.dump()==R"([null,false,true,0,-9223372036854775808,18446744073709551615,{"a":["雪",{"b":"\u0000\b\f\r"}]}])");
    std::string controls;
    for(int i=0;i<32;++i) controls+=char(i);
    assert(Json(controls).dump()==R"("\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f")");
    for(const char* input:{R"({"a":"\q"})",R"({"a":"\uD800"})",R"({"a":01})",R"({"a":1,"a":2})",R"({"outer":{"a":1,"a":2}})",R"([1,])",R"({"a":true}extra)","\"\xc0\xaf\""}) {
        Value invalid;
        assert(Parser(input).parse(invalid)==false);
    }
    Value nested;
    assert(Parser(std::string(64,'[')+"0"+std::string(64,']')).parse(nested));
    assert(Parser(std::string(65,'[')+"0"+std::string(65,']')).parse(nested)==false);
    bool invalidUtf8=false;
    try { Json(std::string("\xed\xa0\x80")).dump(); }
    catch(const Json::type_error&) { invalidUtf8=true; }
    assert(invalidUtf8==true);
}
