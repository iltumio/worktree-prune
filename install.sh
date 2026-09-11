#!/bin/sh
# Build and install this checkout without sudo.
set -eu

case "${1:-}" in
    -h|--help)
        printf '%s\n' 'Usage: ./install.sh' \
            'Builds this checkout and installs worktree-prune into INSTALL_DIR' \
            '(default: $HOME/.local/bin). Requires Rust/Cargo and a C linker.' \
            'An existing, different executable is backed up before replacement.'
        exit 0
        ;;
    '') ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
esac
[ "$#" -le 1 ] || { printf 'Too many arguments\n' >&2; exit 2; }

command -v cargo >/dev/null 2>&1 || {
    printf 'Cargo is required. Install Rust, then run this script again.\n' >&2
    exit 1
}
source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
install_dir=${INSTALL_DIR:-"$HOME/.local/bin"}
mkdir -p -- "$install_dir"
install_dir=$(CDPATH='' cd -- "$install_dir" && pwd -P)
destination="$install_dir/worktree-prune"
[ ! -d "$destination" ] || { printf 'Destination is a directory: %s\n' "$destination" >&2; exit 1; }
staging=$(mktemp -d "$install_dir/.worktree-prune-install.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Use Cargo's installation layout so custom target directories also work.
cargo install --path "$source_dir" --locked --root "$staging" \
    --target-dir "${CARGO_TARGET_DIR:-$source_dir/target}"

if [ -e "$destination" ] || [ -L "$destination" ]; then
    if cmp -s "$staging/bin/worktree-prune" "$destination"; then
        printf 'Already up to date: %s\n' "$destination"
        exit 0
    fi
    backup=$(mktemp "$install_dir/worktree-prune.bak.XXXXXX")
    if ! cp -p -- "$destination" "$backup"; then
        rm -f -- "$backup"
        printf 'Backup failed; existing installation kept.\n' >&2
        exit 1
    fi
    printf 'Backup: %s\n' "$backup"
fi
# Staging and destination are on the same filesystem: replace only after build.
mv -f -- "$staging/bin/worktree-prune" "$destination"
printf 'Installed: %s\n' "$destination"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) printf 'Add this directory to your shell PATH: %s\n' "$install_dir" ;;
esac
