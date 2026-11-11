// SPDX-License-Identifier: MIT
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

static std::string hex(const std::string& bytes) {
    static const char digits[] = "0123456789abcdef";
    std::string result;
    for (unsigned char c : bytes) {
        result += digits[c >> 4];
        result += digits[c & 15];
    }
    return result;
}

static std::vector<std::string> frames(const std::string& text, size_t chunk = 1400,
        unsigned long tid = 0, unsigned long sequence = 1) {
    const size_t parts = std::max(size_t(1), (text.size() + chunk - 1) / chunk);
    std::vector<std::string> result;
    for (size_t i = 0; i < parts; ++i)
        result.push_back("VDBG1|probe|info|default|" + std::to_string(tid) + "|" + std::to_string(sequence)
            + "|" + std::to_string(i + 1) + "|"
            + std::to_string(parts) + "|" + hex(text.substr(i * chunk, chunk)));
    return result;
}

static int self_test() {
    if (hex("\xe4\xb8\xad\xe6\x96\x87\xf0\x9f\x98\x80\n") != "e4b8ade69687f09f98800a"
        || frames("abc", 2) != std::vector<std::string>{
            "VDBG1|probe|info|default|0|1|1|2|6162", "VDBG1|probe|info|default|0|1|2|2|63"}
        || frames("") != std::vector<std::string>{"VDBG1|probe|info|default|0|1|1|1|"}) {
        std::puts("self-test=failed");
        return 1;
    }
    std::puts("self-test=passed");
    return 0;
}

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <thread>

static void emit(const char* mode) {
    static volatile LONG sequence = 0;
    LARGE_INTEGER frequency;
    QueryPerformanceFrequency(&frequency);
    auto send = [&](const std::string& label, const std::string& text) {
        std::vector<wchar_t> wide;
        std::vector<std::string> records;
        if (!std::strcmp(mode, "wide")) {
            int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.c_str(), -1, nullptr, 0);
            wide.resize(count);
            MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.c_str(), -1, wide.data(), count);
        } else if (!std::strcmp(mode, "framed")) {
            records = frames(text, 1400, GetCurrentThreadId(),
                static_cast<unsigned long>(InterlockedIncrement(&sequence)));
        }
        LARGE_INTEGER start, end;
        QueryPerformanceCounter(&start);
        if (!std::strcmp(mode, "wide")) {
            OutputDebugStringW(wide.data());
        } else if (!std::strcmp(mode, "framed")) {
            for (const auto& record : records)
                OutputDebugStringA(record.c_str());
        } else {
            OutputDebugStringA(text.c_str());
        }
        QueryPerformanceCounter(&end);
        std::printf("sample=%s bytes=%zu elapsed_us=%.3f\n", label.c_str(), text.size(),
            (end.QuadPart - start.QuadPart) * 1000000.0 / frequency.QuadPart);
    };
    send("unicode", "probe unicode: \xe4\xb8\xad\xe6\x96\x87 \xf0\x9f\x98\x80\n");
    send("multiline", "probe line 1\nprobe line 2\r\nprobe line 3\n");
    for (size_t size : {4090u, 4091u, 4092u, 4095u, 4096u, 4097u, 8192u}) {
        std::string text = "boundary=" + std::to_string(size) + ":";
        text.resize(size - 5, 'x');
        text += ":END\n";
        send("boundary-" + std::to_string(size), text);
    }
    std::vector<std::thread> threads;
    for (int thread = 0; thread < 4; ++thread)
        threads.emplace_back([&, thread] {
            for (int sequence = 0; sequence < 16; ++sequence) {
                std::string id = "thread=" + std::to_string(thread) + " sequence=" + std::to_string(sequence);
                send(id, "probe " + id + "\n");
            }
        });
    for (auto& thread : threads)
        thread.join();
}

static int receive(unsigned seconds) {
    struct Buffer { DWORD pid; char text[4092]; };
    static_assert(sizeof(Buffer) == 4096);
    HANDLE objects[3] = {};
    const char* names[] = {"DBWIN_BUFFER_READY", "DBWIN_DATA_READY", "DBWIN_BUFFER"};
    for (int i = 0; i < 3; ++i) {
        objects[i] = i == 2 ? CreateFileMappingA(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, 4096, names[i])
                            : CreateEventA(nullptr, FALSE, FALSE, names[i]);
        DWORD error = GetLastError();
        if (!objects[i] || error == ERROR_ALREADY_EXISTS) {
            std::printf("receiver-start object=%s error=%lu\n", names[i], error);
            for (HANDLE object : objects) if (object) CloseHandle(object);
            return 1;
        }
    }
    auto* buffer = static_cast<Buffer*>(MapViewOfFile(objects[2], FILE_MAP_READ, 0, 0, 4096));
    if (!buffer) {
        std::printf("receiver-map error=%lu\n", GetLastError());
        for (HANDLE object : objects) CloseHandle(object);
        return 1;
    }
    ULONGLONG deadline = GetTickCount64() + seconds * 1000ULL;
    SetEvent(objects[0]);
    while (GetTickCount64() < deadline) {
        DWORD wait = WaitForSingleObject(objects[1], 250);
        if (wait == WAIT_TIMEOUT) continue;
        if (wait != WAIT_OBJECT_0) {
            std::printf("receiver-wait error=%lu\n", GetLastError());
            UnmapViewOfFile(buffer);
            for (HANDLE object : objects) CloseHandle(object);
            return 1;
        }
        DWORD pid = buffer->pid;
        size_t length = 0;
        while (length < sizeof(buffer->text) && buffer->text[length]) ++length;
        std::string copy(buffer->text, length);
        SetEvent(objects[0]);
        std::printf("received pid=%lu bytes=%zu terminated=%d hex=%s\n", pid, length,
            int(length < sizeof(buffer->text)), hex(copy).c_str());
        std::fflush(stdout);
    }
    UnmapViewOfFile(buffer);
    for (HANDLE object : objects) CloseHandle(object);
    return 0;
}
#endif

int main(int argc, char** argv) {
    if (argc == 2 && !std::strcmp(argv[1], "--self-test")) return self_test();
#ifdef _WIN32
    if (argc == 3 && !std::strcmp(argv[1], "--emit")
        && (!std::strcmp(argv[2], "utf8") || !std::strcmp(argv[2], "wide") || !std::strcmp(argv[2], "framed"))) {
        emit(argv[2]);
        return 0;
    }
    if (argc == 3 && !std::strcmp(argv[1], "--receive")) {
        char* end;
        unsigned long seconds = std::strtoul(argv[2], &end, 10);
        if (*argv[2] && !*end && seconds > 0 && seconds <= 3600) return receive(seconds);
    }
#endif
    std::puts("usage: debug-output-probe --self-test | --emit utf8|wide|framed | --receive SECONDS");
    return 2;
}
