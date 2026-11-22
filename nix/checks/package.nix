# What the package must contain. On every system: bin/halyard. On Linux also
# the desktop entry and icons, and every library GPUI loads with dlopen must
# resolve through the binary's RUNPATH (they are not in its NEEDED list, so a
# missing one would only show up as a window that never opens). Control: a
# library that is not there must be reported missing, so the resolver cannot
# pass by being broken.
{
  pkgs,
  halyard,
}:
let
  inherit (pkgs) lib stdenv;
  dlopened = [
    "libvulkan.so.1"
    "libwayland-client.so.0"
    "libwayland-cursor.so.0"
    "libwayland-egl.so.1"
    "libxkbcommon.so.0"
    "libxkbcommon-x11.so.0"
    "libfontconfig.so.1"
    "libfreetype.so.6"
    "libX11.so.6"
    "libX11-xcb.so.1"
    "libxcb.so.1"
    "libXcursor.so.1"
    "libXi.so.6"
    "libXrandr.so.2"
  ];
in
pkgs.runCommand "halyard-package-contents"
  {
    nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [
      pkgs.patchelf
      pkgs.desktop-file-utils
    ];
  }
  (
    ''
      pkg=${halyard}
      echo "package: $pkg"
      find "$pkg" -mindepth 1 | sed "s|^$pkg/||" | sort
      test -x "$pkg/bin/halyard"
      echo "ok: bin/halyard"
    ''
    + lib.optionalString stdenv.hostPlatform.isLinux ''
      desktop-file-validate "$pkg/share/applications/halyard.desktop"
      grep -qx 'StartupWMClass=halyard' "$pkg/share/applications/halyard.desktop"
      test -f "$pkg/share/icons/hicolor/scalable/apps/halyard.svg"
      for size in 16 32 48 64 128 256 512; do
        test -s "$pkg/share/icons/hicolor/''${size}x''${size}/apps/halyard.png"
      done
      echo "ok: desktop entry and icons"

      rpath="$(patchelf --print-rpath "$pkg/bin/halyard")"
      resolve() {
        local dir
        IFS=: read -ra dirs <<< "$rpath"
        for dir in "''${dirs[@]}"; do
          if [ -e "$dir/$1" ]; then
            echo "$dir/$1"
            return 0
          fi
        done
        return 1
      }
      missing=0
      for so in ${lib.concatStringsSep " " dlopened}; do
        if found="$(resolve "$so")"; then
          echo "ok: $so -> $found"
        else
          echo "MISSING: $so" >&2
          missing=1
        fi
      done
      [ "$missing" -eq 0 ]

      if resolve libhalyard-control-not-real.so.1 >/dev/null; then
        echo "control failed: a library that does not exist resolved" >&2
        exit 1
      fi
      echo "control ok: a missing library is reported missing"
    ''
    + ''
      touch "$out"
    ''
  )
