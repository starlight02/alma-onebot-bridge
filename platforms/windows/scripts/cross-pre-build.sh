#!/usr/bin/env bash
set -euo pipefail

ln -sf /usr/bin/x86_64-w64-mingw32-windres /usr/local/bin/windres

# cross-rs 的 x86_64-pc-windows-gnu 镜像基于 Ubuntu 20.04，其 mingw-w64 9.3 的
# winerror.h 只定义到 ERROR_COMMITMENT_LIMIT(1455)，缺 mimalloc v3/v2 引用的
# ERROR_COMMITMENT_MINIMUM(635)。补一个兼容定义让 mimalloc 能编过。
WINERROR_H=/usr/x86_64-w64-mingw32/include/winerror.h
if ! grep -q 'ERROR_COMMITMENT_MINIMUM' "$WINERROR_H" 2>/dev/null; then
  cat >> "$WINERROR_H" <<'EOF'

#ifndef ERROR_COMMITMENT_MINIMUM
#define ERROR_COMMITMENT_MINIMUM __MSABI_LONG(635)
#endif
EOF
fi

cat >/tmp/gethostnamew_shim.c <<'EOF'
#include <winsock2.h>
#include <windows.h>

// cross-rs 镜像基于 Ubuntu 20.04，其 mingw-w64 7.3/9.3 的 libws2_32.a 不导出
// GetHostNameW（Rust std::sys::net::hostname 引用），导致 turso_sdk_kit 的
// cdylib 链接失败。这里提供强定义补上。weak 在 -shared cdylib 下不被链接器
// 拉取，所以必须用非 weak 的普通定义。
int WSAAPI GetHostNameW(PWSTR name, int namelen) {
    DWORD size;

    if (name == NULL || namelen <= 0) {
        WSASetLastError(WSAEFAULT);
        return SOCKET_ERROR;
    }

    size = (DWORD)namelen;
    if (GetComputerNameW(name, &size)) {
        return 0;
    }

    WSASetLastError(WSAEFAULT);
    return SOCKET_ERROR;
}
EOF

x86_64-w64-mingw32-gcc -Wall -Wextra -c /tmp/gethostnamew_shim.c \
  -o /usr/local/lib/gethostnamew_shim.o
