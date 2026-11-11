// SPDX-License-Identifier: MIT
#pragma once
#include <nlohmann/json.hpp>
#include <set>
#include <string>
#include <vector>

namespace panel::debugjson {
using Json = nlohmann::ordered_json;

struct Value {
    enum { Null, Bool, Number, String, Array, Object } type = Null;
    Json json;
    std::string text;
    bool boolean = false;
    std::vector<Value> items;
    std::vector<std::pair<std::string, Value>> members;

    Value() = default;
    explicit Value(Json value) : json(std::move(value)) {
        if(json.is_string()) { type=String; text=json.get<std::string>(); }
        else if(json.is_number()) { type=Number; text=json.dump(); }
        else if(json.is_boolean()) { type=Bool; boolean=json.get<bool>(); }
        else if(json.is_array()) {
            type=Array;
            for(const auto& item:json) items.emplace_back(item);
        } else if(json.is_object()) {
            type=Object;
            for(const auto& member:json.items()) members.emplace_back(member.key(),Value(member.value()));
        }
    }
    const Value& operator[](const char* key) const {
        static const Value empty;
        for(const auto& member:members) if(member.first==key) return member.second;
        return empty;
    }
};

class Parser {
public:
    explicit Parser(const std::string& input) : input_(input) {}
    bool parse(Value& value) {
        std::vector<std::set<std::string>> keys;
        bool valid=true;
        auto callback=[&](int depth, Json::parse_event_t event, Json& parsed) {
            if(depth>64) valid=false;
            if(event==Json::parse_event_t::object_start) keys.emplace_back();
            else if(event==Json::parse_event_t::key) {
                if(!keys.back().insert(parsed.get<std::string>()).second) valid=false;
            } else if(event==Json::parse_event_t::object_end) keys.pop_back();
            return true;
        };
        auto parsed=Json::parse(input_,callback,false);
        if(!valid || parsed.is_discarded()) return false;
        value=Value(std::move(parsed));
        return true;
    }
private:
    std::string input_;
};
}
