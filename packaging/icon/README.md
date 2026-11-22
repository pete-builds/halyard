# Icon

`halyard.svg` is the source: original artwork (a mast, its halyard line and a pennant over a
prompt cursor), MIT OR Apache-2.0, no third-party art. It is the same file as
`dist/icons/halyard.svg` in the Nix flake PR.

`halyard-1024.png` is rendered from it with transparent corners, and is what
`packaging/macos/build-app.sh` turns into `Halyard.icns` (`sips` for the sizes, `iconutil`
for the container; both ship with macOS). It is committed so the macOS build needs no
SVG tool. To regenerate it after editing the SVG, any renderer that keeps transparency works.
This one was made with headless Chrome:

```sh
cat > /tmp/icon.html <<HTML
<html><body style="margin:0;background:transparent">
<img src="file://$PWD/packaging/icon/halyard.svg" width="1024" height="1024" style="display:block">
</body></html>
HTML
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new \
  --default-background-color=00000000 --window-size=1024,1024 \
  --screenshot=packaging/icon/halyard-1024.png file:///tmp/icon.html
```

`rsvg-convert -w 1024 -h 1024 halyard.svg -o halyard-1024.png` gives the same result.
`qlmanage -t` is not suitable: it flattens the transparent corners onto white.
