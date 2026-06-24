#!/usr/bin/env bash
set -euo pipefail

ln -sf /usr/bin/x86_64-w64-mingw32-windres /usr/local/bin/windres

cat >/tmp/check_gethostnamew.c <<'EOF'
#include <winsock2.h>
#include <windows.h>

int main(void) {
    WCHAR name[256];
    return GetHostNameW(name, 256);
}
EOF

if x86_64-w64-mingw32-gcc /tmp/check_gethostnamew.c -o /tmp/check_gethostnamew.exe -lws2_32 >/dev/null 2>&1; then
  cat >/tmp/gethostnamew_empty.c <<'EOF'
/* MinGW already provides GetHostNameW in ws2_32. */
EOF
  x86_64-w64-mingw32-gcc -c /tmp/gethostnamew_empty.c \
    -o /usr/local/lib/gethostnamew_shim.o
  exit 0
fi

cat >/tmp/gethostnamew_shim.c <<'EOF'
#include <winsock2.h>
#include <windows.h>

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
