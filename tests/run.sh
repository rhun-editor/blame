#!/bin/sh
# Builds the extension for this machine, lays it out as rhun installs it, makes a Git repository
# with two commits by two authors, and runs tests/basic.txt headlessly against a rhun binary: the
# cursor line's blame, the hover card, an unsaved edit, and a line's history opening rhun's diff.
# Ages ("3 years ago") are written as AGE. usage: tests/run.sh [RHUN]
set -e
cd "$(dirname "$0")/.."
RHUN=${1:-$(command -v rhun || echo "$HOME/.local/bin/rhun")}
name=$(sed -n 's/^name *= *//p' extension.ini | head -1)
owner=$(sed -n 's/^owner *= *//p' extension.ini | head -1)
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=linux-x86_64 ;;
    Darwin-arm64) target=macos-arm64 ;;
    *) echo "no target for $(uname -s)-$(uname -m)"; exit 77 ;;
esac
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'rm -rf "$work"' EXIT
dir=$work/exts/$owner/$name
mkdir -p "$dir/bin" "$work/proj" "$work/config/rhun" "$work/state"
sh tests/layout.sh "$dir"  # what the release ships: the manifest, the icon, README.md, LICENSE
cargo build --release --quiet
cp "target/release/$name" "$dir/bin/$name-$target"
(
    cd "$work/proj"
    export GIT_CONFIG_NOSYSTEM=1 HOME="$work"
    git init -q -b main
    git config user.name Ada
    git config user.email ada@example.invalid
    printf 'one\ntwo\nthree\n' > a.txt
    git add a.txt
    GIT_AUTHOR_DATE='2001-02-03T04:05:06Z' git commit -qm 'First lines'
    printf 'one\nTWO\nthree\n' > a.txt
    GIT_AUTHOR_NAME=Grace GIT_AUTHOR_EMAIL=grace@example.invalid GIT_AUTHOR_DATE='2002-03-04T05:06:07Z' \
        git commit -qam 'Shout the second line'
)
printf '[files]\nrestore_session = false\nrestore_project = false\n[updates]\ncheck = false\n[git]\nenabled = true\n[ui]\nsidebar = false\nagents_panel = false\n[editor]\ncursor_blink = false\n' > "$work/config/rhun/config"
sed "s|proj/|$work/proj/|" tests/basic.txt > "$work/basic.txt"
HOME=$work XDG_CONFIG_HOME=$work/config XDG_STATE_HOME=$work/state GIT_CONFIG_NOSYSTEM=1 \
    "$RHUN" "$work/proj" --headless 1000x600 --scale 1 --extensions "$work/exts" --script "$work/basic.txt" 2>&1 \
    | sed -E -e 's/pid=[1-9][0-9]*/pid=PID/' -e 's/[0-9]+ (year|month|day|hour|minute)s? ago/AGE/g' \
          -e 's/\([0-9a-f]{7}\.\.[0-9a-f]{7}\)/(SHA..SHA)/' -e 's/^([> ] )[0-9a-f]{7}  /\1SHA  /' \
          -e 's/Commit: [0-9a-f]+/Commit: SHA/' \
          -e 's/^[0-9a-f]{7} /SHA /' > "$work/out.txt"
if cmp -s "$work/out.txt" tests/basic.expected || ([ -f tests/basic.legacy.expected ] && cmp -s "$work/out.txt" tests/basic.legacy.expected); then
    echo "ok   basic"
else
    echo "FAIL basic"; diff tests/basic.expected "$work/out.txt" || true; [ -z "$KEEP_OUT" ] || cp "$work/out.txt" "$KEEP_OUT"
    echo "--- the extension's log:"; cat "$work"/state/rhun/extensions/*.log 2>/dev/null | tail -20; exit 1
fi
