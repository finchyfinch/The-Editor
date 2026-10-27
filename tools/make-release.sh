#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Build a Linux release of The Editor and stage it for distribution. The
# Linux counterpart of tools\make-release.bat; on Windows, run it under WSL:
#
#     wsl -d Ubuntu -- tools/make-release.sh [REF]
#
# REF is the commit or tag to build, HEAD by default. The build happens in a
# clone on the Linux filesystem, not in this checkout. Building across
# /mnt/c is several times slower, and a Linux build sharing target/ with the
# Windows one would leave both rebuilding each other's dependencies. A clone
# also means what is built is exactly what was committed: no uncommitted
# edit can slip into a release, and the commit stamped into the About box is
# the one the release is tagged at.
#
# Produces, in dist/:
#     the-editor-<version>-linux-x64.tar.gz          what you actually send
#     the-editor-<version>-linux-x64.tar.gz.sha256
#
# A tarball rather than a zip, because it keeps the executable bit.
#
# Needs, once: rustup, the development packages winit and glutin link
# against, and python3-venv, without which Ubuntu's Python cannot make the
# virtual environment one of the tests creates. On Ubuntu:
#     sudo apt-get install pkg-config zip libxkbcommon-dev libwayland-dev \
#         libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl-dev libegl-dev \
#         python3-venv
# ---------------------------------------------------------------------------
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
REF="${1:-HEAD}"
WORK="${XDG_CACHE_HOME:-$HOME/.cache}/the-editor-release"
SRC="$WORK/src"

# rustup's installer adds this to the profile, which `wsl -- cmd` never reads.
export PATH="$HOME/.cargo/bin:$PATH"

# Keep one clone and fetch into it, so target/ survives between releases and
# the next build is incremental. safe.directory because the Windows checkout
# is owned by a different user as far as Linux is concerned.
if [ ! -d "$SRC/.git" ]; then
    mkdir -p "$WORK"
    git -c safe.directory='*' clone --quiet "$REPO" "$SRC"
fi
git -C "$SRC" -c safe.directory='*' fetch --quiet --tags "$REPO" "+refs/heads/*:refs/remotes/local/*"
COMMIT="$(git -C "$REPO" -c safe.directory='*' rev-parse --verify "$REF^{commit}")"
git -C "$SRC" checkout --quiet --detach "$COMMIT"

# The version comes from the workspace manifest at that commit, so the
# tarball cannot be named after the wrong release.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$SRC/Cargo.toml" | head -1)"
if [ -z "$VERSION" ]; then
    echo "Could not read the version from Cargo.toml." >&2
    exit 1
fi
NAME="the-editor-$VERSION-linux-x64"

cd "$SRC"
echo "Building The Editor $VERSION ($(git rev-parse --short HEAD)) ..."
cargo build --release --locked --package the-editor

# The same suite, for the same reasons, as make-release.bat.
echo "Running the test suite ..."
cargo test --workspace --all-targets --quiet --locked

STAGE="$WORK/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp target/release/the-editor "$STAGE/"
cp LICENSE "$STAGE/LICENSE.txt"
cp CHANGELOG.md "$STAGE/CHANGELOG.txt"
cp README.md "$STAGE/README.txt"

cat > "$STAGE/FIRST-RUN.txt" <<EOF
The Editor $VERSION - portable build for Linux x64

Run ./the-editor. There is nothing to install: everything the program needs
is inside that one file, and it writes its settings to ~/.config the first
time it starts.

It needs a graphical desktop (X11 or Wayland) with OpenGL, and a glibc at
least as new as the one it was built against ($(ldd --version | head -1 | sed 's/.* //')).
Open and Save dialogs come from xdg-desktop-portal, which desktop
distributions install by default.

Check the published SHA-256 of the tarball first if you did not build it
yourself.

Optional, and detected automatically if present:
  pip install ruff           linting
  pip install basedpyright   types, completion, go to definition
  pip install debugpy        the debugger
  rustup component add rust-analyzer

Help - Check Toolchains lists what was found.
EOF

echo "Packaging ..."
mkdir -p "$REPO/dist"
tar -C "$WORK" -czf "$REPO/dist/$NAME.tar.gz" "$NAME"
(cd "$REPO/dist" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
rm -rf "$STAGE"

echo
echo "Done."
ls -l "$REPO/dist/$NAME.tar.gz"
cat "$REPO/dist/$NAME.tar.gz.sha256"
