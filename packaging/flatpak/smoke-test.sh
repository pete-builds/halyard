#!/bin/sh
# Smoke test for the Flatpak build, run without a display:
#   - the binary exists and is executable;
#   - every library it links resolves (no "not found" from ldd);
#   - the libraries GPUI loads at run time with dlopen are present;
#   - flatpak-spawn exists, since Halyard starts the user's shell through it.
#
# The manifest runs it inside the SDK at the end of the build. CI runs it again
# inside the Platform runtime after installing the bundle, which is the
# environment users get:
#   flatpak run --command=sh io.github.pete_builds.Halyard -c "$(cat smoke-test.sh)" sh /app/bin/halyard
#
# It does not open a window: that needs a display and a GPU (see docs/flatpak.md).
set -eu

bin="${1:-/app/bin/halyard}"
fail=0

if [ ! -x "$bin" ]; then
  echo "FAIL: $bin is missing or not executable" >&2
  exit 1
fi
echo "ok: $bin is executable"

out="$(ldd "$bin")"
echo "$out"
if echo "$out" | grep -q 'not found'; then
  echo "FAIL: ldd reports missing libraries:" >&2
  echo "$out" | grep 'not found' >&2
  fail=1
else
  echo "ok: every linked library resolves"
fi

# Loaded with dlopen by GPUI's Linux backends, so ldd does not list them all.
dlopened="libvulkan.so.1 libwayland-client.so.0 libwayland-cursor.so.0 libxkbcommon.so.0 libxkbcommon-x11.so.0 libX11.so.6 libX11-xcb.so.1 libxcb.so.1 libXcursor.so.1 libXi.so.6 libXrandr.so.2 libfontconfig.so.1 libfreetype.so.6"
cache="$(ldconfig -p 2>/dev/null || true)"
for lib in $dlopened; do
  found=""
  if echo "$cache" | grep -q " $lib "; then
    found="ld.so.cache"
  else
    for dir in /app/lib /usr/lib/*-linux-gnu /usr/lib64 /usr/lib; do
      if [ -e "$dir/$lib" ]; then
        found="$dir"
        break
      fi
    done
  fi
  if [ -n "$found" ]; then
    echo "ok: $lib ($found)"
  else
    echo "FAIL: $lib not found" >&2
    fail=1
  fi
done

# Halyard passes the host program's environment to flatpak-spawn through a
# /bin/sh step (crates/halyard-term/src/flatpak.rs).
if [ -x /bin/sh ]; then
  echo "ok: /bin/sh is executable"
else
  echo "FAIL: /bin/sh is missing, so Halyard cannot hand flatpak-spawn the environment" >&2
  fail=1
fi

if command -v flatpak-spawn >/dev/null 2>&1; then
  echo "ok: flatpak-spawn at $(command -v flatpak-spawn)"
else
  echo "FAIL: flatpak-spawn is missing, so Halyard cannot start host shells" >&2
  fail=1
fi

exit "$fail"
