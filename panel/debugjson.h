// SPDX-License-Identifier: MIT
#pragma once
#include <string>
#include <vector>
#include <utility>
#include <cstring>
#include <cctype>

namespace panel::debugjson {
struct Value {
    enum { Null, Bool, Number, String, Array, Object } type = Null;
    std::string text;
    bool boolean = false;
    std::vector<Value> items;
    std::vector<std::pair<std::string, Value>> members;
    const Value& operator[](const char* key) const {
        static const Value empty;
        for (const auto& m : members) if (m.first == key) return m.second;
        return empty;
    }
};
class Parser {
public:
    explicit Parser(const std::string& s) : p(s.data()), end(p+s.size()) {}
    bool parse(Value& v) { bool ok = value(v, 0); space(); return ok && p == end; }
private:
    const char *p, *end;
    void space() { while (p < end && (*p==' ' || *p=='\r' || *p=='\n' || *p=='\t')) ++p; }
    bool take(char c) { space(); if (p==end || *p!=c) return false; ++p; return true; }
    bool hex(unsigned& n) {
        n=0;
        for (int i=0;i<4;++i) {
            if(p==end) return false;
            char c=*p++; unsigned d;
            if(c>='0'&&c<='9') d=c-'0'; else if(c>='a'&&c<='f') d=c-'a'+10;
            else if(c>='A'&&c<='F') d=c-'A'+10; else return false;
            n=n*16+d;
        }
        return true;
    }
    static void utf8(std::string& s, unsigned c) {
        if(c<128) s+=char(c);
        else if(c<2048) { s+=char(0xc0|(c>>6)); s+=char(0x80|(c&63)); }
        else if(c<65536) { s+=char(0xe0|(c>>12)); s+=char(0x80|((c>>6)&63)); s+=char(0x80|(c&63)); }
        else { s+=char(0xf0|(c>>18)); s+=char(0x80|((c>>12)&63)); s+=char(0x80|((c>>6)&63)); s+=char(0x80|(c&63)); }
    }
    bool string(std::string& s) {
        if(!take('"')) return false;
        while(p<end) {
            unsigned char c=*p++;
            if(c=='"') return true;
            if(c<32) return false;
            if(c!='\\') { s+=char(c); continue; }
            if(p==end) return false;
            switch(*p++) {
            case '"': s+='"'; break; case '\\': s+='\\'; break; case '/': s+='/'; break;
            case 'b': s+='\b'; break; case 'f': s+='\f'; break; case 'n': s+='\n'; break;
            case 'r': s+='\r'; break; case 't': s+='\t'; break;
            case 'u': {
                unsigned n, low; if(!hex(n)) return false;
                if(n>=0xd800 && n<=0xdbff) {
                    if(end-p<2 || *p++!='\\' || *p++!='u' || !hex(low) || low<0xdc00 || low>0xdfff) return false;
                    n=65536+((n-0xd800)<<10)+(low-0xdc00);
                } else if(n>=0xdc00 && n<=0xdfff) return false;
                utf8(s,n); break;
            }
            default: return false;
            }
        }
        return false;
    }
    bool value(Value& v, int depth) {
        space(); if(p==end || depth>64) return false;
        if(*p=='{') {
            ++p; v.type=Value::Object; if(take('}')) return true;
            do {
                std::string key; if(!string(key)||!take(':')) return false;
                for(const auto& m:v.members) if(m.first==key) return false;
                Value child; if(!value(child,depth+1)) return false;
                v.members.emplace_back(std::move(key),std::move(child));
                if(take('}')) return true;
            } while(take(',')); return false;
        }
        if(*p=='[') {
            ++p; v.type=Value::Array; if(take(']')) return true;
            do { Value child; if(!value(child,depth+1)) return false; v.items.push_back(std::move(child));
                if(take(']')) return true;
            } while(take(',')); return false;
        }
        if(*p=='"') { v.type=Value::String; return string(v.text); }
        for(const char* word:{"true","false","null"}) {
            size_t n=std::strlen(word); if(size_t(end-p)>=n && !std::memcmp(p,word,n)) {
                p+=n; v.type=word[0]=='n'?Value::Null:Value::Bool; v.boolean=word[0]=='t'; return true;
            }
        }
        const char* start=p;
        if(*p=='-') ++p;
        if(p==end) return false;
        if(*p=='0') ++p;
        else { if(*p<'1'||*p>'9') return false; while(p<end && std::isdigit((unsigned char)*p)) ++p; }
        if(p<end && *p=='.') { ++p; const char* q=p; while(p<end&&std::isdigit((unsigned char)*p)) ++p; if(p==q) return false; }
        if(p<end&&(*p=='e'||*p=='E')) { ++p; if(p<end&&(*p=='+'||*p=='-')) ++p;
            const char* q=p; while(p<end&&std::isdigit((unsigned char)*p)) ++p; if(p==q) return false; }
        v.type=Value::Number; v.text.assign(start,p); return true;
    }
};
inline std::string quote(const std::string& text) {
    std::string out="\"";
    for(unsigned char c:text) {
        if(c=='"'||c=='\\') { out+='\\'; out+=char(c); }
        else if(c<32) { const char* hex="0123456789abcdef"; out+="\\u00"; out+=hex[c>>4]; out+=hex[c&15]; }
        else out+=char(c);
    }
    return out+'"';
}
inline std::string encode(const Value& v) {
    if(v.type==Value::String) return quote(v.text);
    if(v.type==Value::Number) return v.text;
    if(v.type==Value::Bool) return v.boolean?"true":"false";
    if(v.type==Value::Null) return "null";
    std::string out=v.type==Value::Array?"[":"{";
    if(v.type==Value::Array) for(const auto& item:v.items) {
        if(out.size()>1) out+=',';
        out+=encode(item);
    }
    else for(const auto& member:v.members) {
        if(out.size()>1) out+=',';
        out+=quote(member.first)+":"+encode(member.second);
    }
    return out+(v.type==Value::Array?"]":"}");
}
}
