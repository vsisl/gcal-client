#!/bin/sh
# Packs a built gcal-client into an AppImage.
#
#   build-appimage.sh <binary> <version> <output folder> [<update information>]
#
# Needs `appimagetool` on the PATH (https://github.com/AppImage/appimagetool) and,
# with update information, `zsyncmake` (package "zsync").
#
# Nothing but the binary goes in. It links to the C library and fontconfig and loads
# the desktop's own Wayland/X11, keyboard and OpenGL libraries when it starts: all of
# them belong to the system the AppImage runs on, and the AppImage project's list of
# libraries not to bundle names them. Build the binary on an old distribution, since
# it then runs on that one and every newer one.
#
# The update information is written into the AppImage, and a .zsync file is made
# beside it: with both published, tools such as Gear Lever find and fetch a newer
# version. See .github/workflows/release.yml for the line used for releases.
set -eu

binary=$1
version=$2
out=$3
update=${4:-}

id=io.github.vsisl.gcal_client
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/AppDir"

install -Dm755 "$binary" "$app/usr/bin/gcal-client"
install -Dm644 "$here/$id.desktop" "$app/usr/share/applications/$id.desktop"
install -Dm644 "$here/$id.metainfo.xml" "$app/usr/share/metainfo/$id.metainfo.xml"
install -Dm644 "$here/$id.svg" "$app/usr/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm644 "$here/$id.png" "$app/usr/share/icons/hicolor/256x256/apps/$id.png"
# What the AppImage format wants at the top: the launcher's entry, its icon, and the
# program to start.
ln -s "usr/share/applications/$id.desktop" "$app/$id.desktop"
ln -s "usr/share/icons/hicolor/256x256/apps/$id.png" "$app/$id.png"
ln -s "$id.png" "$app/.DirIcon"
cat > "$app/AppRun" <<'RUN'
#!/bin/sh
exec "$(dirname "$(readlink -f "$0")")/usr/bin/gcal-client" "$@"
RUN
chmod 755 "$app/AppRun"

mkdir -p "$out"
out=$(cd "$out" && pwd)
name="gcal-client-$version-x86_64.AppImage"
# appimagetool writes the .zsync file into the folder it is run in.
cd "$out"
if [ -n "$update" ]; then
    ARCH=x86_64 VERSION="$version" appimagetool --updateinformation "$update" "$app" "$name"
else
    ARCH=x86_64 VERSION="$version" appimagetool "$app" "$name"
fi
echo "$out/$name"
