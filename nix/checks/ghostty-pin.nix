# The flake's Ghostty input must be the commit libghostty-vt-sys pins.
# Reads the libghostty-vt-sys version from Cargo.lock, finds that crate's
# build.rs in the vendored dependencies, and runs nix/check-ghostty-pin.sh.
# It also runs the script with a wrong revision and requires it to fail, so
# the check cannot pass by being broken.
{
  pkgs,
  src,
  cargoVendorDir,
  ghosttyRev,
}:
let
  lock = builtins.fromTOML (builtins.readFile (src + "/Cargo.lock"));
  sysPackages = builtins.filter (p: p.name == "libghostty-vt-sys") lock.package;
  sysVersion =
    if builtins.length sysPackages == 1 then
      (builtins.head sysPackages).version
    else
      throw "ghostty-pin: expected exactly one libghostty-vt-sys in Cargo.lock, found ${toString (builtins.length sysPackages)}";
  script = ../check-ghostty-pin.sh;
in
pkgs.runCommand "halyard-ghostty-pin" { } ''
  build_rs="$(find -L ${cargoVendorDir} -path '*/libghostty-vt-sys-${sysVersion}/build.rs' -print -quit)"
  if [ -z "$build_rs" ]; then
    echo "error: libghostty-vt-sys-${sysVersion} is not in the vendored crates" >&2
    exit 1
  fi

  bash ${script} ${ghosttyRev} "$build_rs"

  # Control: a different revision must be rejected.
  if bash ${script} 0000000000000000000000000000000000000000 "$build_rs"; then
    echo "control failed: check-ghostty-pin.sh accepted a wrong revision" >&2
    exit 1
  fi
  echo "control ok: a wrong revision is rejected"

  touch "$out"
''
