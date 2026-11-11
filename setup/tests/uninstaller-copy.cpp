// SPDX-License-Identifier: MIT
#include "../uninstaller-copy.h"

#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <string>

namespace fs = std::filesystem;

namespace {

void expect(bool condition, const char* message)
{
    if (!condition)
        throw std::runtime_error(message);
}

struct TempDirectory {
    fs::path path;

    TempDirectory()
    {
        wchar_t root[MAX_PATH], name[MAX_PATH];
        expect(GetTempPathW(MAX_PATH, root) > 0, "temporary directory path");
        expect(GetTempFileNameW(root, L"vuc", 0, name) != 0, "temporary directory name");
        expect(DeleteFileW(name) && CreateDirectoryW(name, nullptr), "temporary directory creation");
        path = name;
    }

    ~TempDirectory()
    {
        std::error_code ec;
        for (const auto& entry : fs::directory_iterator(path, ec))
            SetFileAttributesW(entry.path().c_str(), FILE_ATTRIBUTE_NORMAL);
        fs::remove_all(path, ec);
    }
};

void write(const fs::path& path, const char* content)
{
    std::ofstream file(path, std::ios::binary);
    file << content;
    file.close();
    expect(!file.fail(), "writing file content");
}

std::string read(const fs::path& path)
{
    std::ifstream file(path, std::ios::binary);
    expect(file.is_open(), "reading file content");
    return {std::istreambuf_iterator<char>(file), std::istreambuf_iterator<char>()};
}

void attributes(const fs::path& path, DWORD value)
{
    expect(SetFileAttributesW(path.c_str(), value), "setting file attributes");
}

void expect_writable(const fs::path& path)
{
    DWORD value = GetFileAttributesW(path.c_str());
    expect(value != INVALID_FILE_ATTRIBUTES, "reading copied file attributes");
    expect((value & (FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN)) == 0, "copied file attributes");
    expect((value & FILE_ATTRIBUTE_ARCHIVE) != 0, "copied archive attribute");
    std::ofstream file(path, std::ios::binary | std::ios::app);
    file << " writable";
    file.close();
    expect(!file.fail(), "writing copied file");
    expect(read(path) == "new writable", "updated copied content");
}

} // namespace

int main()
{
    try {
        TempDirectory temp;
        fs::path source = temp.path / L"source.exe";
        fs::path destination = temp.path / L"uninstaller.exe";
        write(source, "new");
        attributes(source, FILE_ATTRIBUTE_ARCHIVE);
        expect(copy_uninstaller(source.c_str(), destination.c_str()) == ERROR_SUCCESS, "first copy");
        expect(read(destination) == "new", "first copy content");
        expect_writable(destination);

        expect(DeleteFileW(destination.c_str()), "removing first copy");
        attributes(source, FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_ARCHIVE);
        expect(copy_uninstaller(source.c_str(), destination.c_str()) == ERROR_SUCCESS, "readonly source copy");
        expect(read(destination) == "new", "readonly source copy content");
        expect_writable(destination);

        write(destination, "old");
        attributes(destination, FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_ARCHIVE);
        expect(copy_uninstaller(source.c_str(), destination.c_str()) == ERROR_SUCCESS, "existing file upgrade");
        expect(read(destination) == "new", "upgraded file content");
        expect_writable(destination);

        write(destination, "old");
        attributes(destination, FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_ARCHIVE);
        DWORD original = GetFileAttributesW(destination.c_str());
        fs::path missing = temp.path / L"missing.exe";
        expect(copy_uninstaller(missing.c_str(), destination.c_str()) == ERROR_FILE_NOT_FOUND, "missing source error");
        expect(read(destination) == "old", "existing content after failed copy");
        expect(GetFileAttributesW(destination.c_str()) == original, "restored file attributes");
        std::cout << "4 uninstaller copy scenarios passed\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
