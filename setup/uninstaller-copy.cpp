// SPDX-License-Identifier: MIT
#include "uninstaller-copy.h"

namespace {

DWORD prepare_attributes(const wchar_t* path, DWORD attributes)
{
    DWORD writable = attributes & ~(FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN);
    if (writable != attributes && !SetFileAttributesW(path, writable ? writable : FILE_ATTRIBUTE_NORMAL))
        return GetLastError();
    return ERROR_SUCCESS;
}

} // namespace

DWORD copy_uninstaller(const wchar_t* source, const wchar_t* destination)
{
    DWORD original = GetFileAttributesW(destination);
    if (original == INVALID_FILE_ATTRIBUTES) {
        DWORD error = GetLastError();
        if (error != ERROR_FILE_NOT_FOUND && error != ERROR_PATH_NOT_FOUND)
            return error;
    } else {
        DWORD error = prepare_attributes(destination, original);
        if (error != ERROR_SUCCESS)
            return error;
    }

    if (!CopyFileW(source, destination, FALSE)) {
        DWORD error = GetLastError();
        if (original != INVALID_FILE_ATTRIBUTES)
            SetFileAttributesW(destination, original);
        return error;
    }

    DWORD copied = GetFileAttributesW(destination);
    if (copied == INVALID_FILE_ATTRIBUTES)
        return GetLastError();
    return prepare_attributes(destination, copied);
}
