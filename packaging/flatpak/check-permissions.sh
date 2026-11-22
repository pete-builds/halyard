#!/bin/sh
# Check an installed Halyard Flatpak's permissions against the documented set
# (docs/flatpak.md), exactly: a missing permission fails, and so does any extra
# one, network access included.
#
# Usage: flatpak info --show-permissions io.github.pete_builds.Halyard | check-permissions.sh
#
# Flatpak's output is an INI file whose list values end in ";" in no fixed
# order, so each entry is normalised to "[Section] key=a;b" with sorted values.
set -eu

expected='[Context] devices=dri
[Context] filesystems=host
[Context] shared=ipc
[Context] sockets=fallback-x11;wayland
[Session Bus Policy] org.freedesktop.Flatpak=talk'

actual="$(awk '
  /^\[.*\]$/ { section = $0; next }
  /^[[:space:]]*$/ { next }
  {
    eq = index($0, "=")
    key = substr($0, 1, eq - 1)
    n = split(substr($0, eq + 1), parts, ";")
    m = 0
    for (i = 1; i <= n; i++) if (parts[i] != "") vals[++m] = parts[i]
    for (i = 2; i <= m; i++) {
      v = vals[i]; j = i - 1
      while (j > 0 && vals[j] > v) { vals[j + 1] = vals[j]; j-- }
      vals[j + 1] = v
    }
    out = ""
    for (i = 1; i <= m; i++) out = out (i > 1 ? ";" : "") vals[i]
    print section " " key "=" out
  }
' | LC_ALL=C sort)"

if [ "$actual" = "$expected" ]; then
  echo "ok: permissions are exactly the documented set"
  echo "$actual"
  exit 0
fi

echo "FAIL: permissions differ from the documented set" >&2
echo "expected:" >&2
echo "$expected" >&2
echo "actual:" >&2
echo "$actual" >&2
exit 1
