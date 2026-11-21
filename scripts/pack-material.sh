#!/usr/bin/env bash
# Pack the Material Icon Theme set that lvim-icons bundles (its assets/material: the SVGs,
# material-icons.json, VERSION, LICENSE) into assets/material.tar.gz, which lvim-fuzzy-ui embeds — so
# lvim-fuzzy and lvim-commander carry the icons themselves and install them into the shared place
# ($XDG_DATA_HOME/lvim-icons/material) without lvim-icons being installed. lvim-icons stays the one
# source: run this after its scripts/update-material.sh.
#
#   scripts/pack-material.sh [path/to/lvim-icons]     # default: ../lvim-icons
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="${1:-$root/../lvim-icons}/assets/material"
[ -f "$src/VERSION" ] || { echo "no Material set at $src" >&2; exit 1; }
# a stable archive: sorted names, no owners / times, so a re-pack of the same set is the same bytes
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='1970-01-01' \
    -C "$src" -cf - . | gzip -9n >"$root/assets/material.tar.gz"
echo "Material $(cat "$src/VERSION"): $(du -h "$root/assets/material.tar.gz" | cut -f1) in assets/material.tar.gz"
