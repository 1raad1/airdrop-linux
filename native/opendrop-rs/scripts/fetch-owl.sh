#!/usr/bin/env bash
# Fetch the SEEMOO `owl` reference (GPLv3) into ./owl/ for cross-checking the
# filin-rs port against the original AWDL implementation.
#
# `owl` is GPLv3, so it is intentionally NOT vendored into this MIT-licensed
# repository — ./owl/ is .gitignored. This script clones it on demand. You do
# not need it to build or run filin-rs / luftlift-rs; it is reference only.
set -euo pipefail
cd "$(dirname "$0")/.."

DEST=owl
REPO=https://github.com/seemoo-lab/owl.git

if [ -d "$DEST/.git" ]; then
  echo "owl already present in ./$DEST — pulling latest"
  git -C "$DEST" pull --ff-only
else
  echo "cloning $REPO into ./$DEST (GPLv3, reference only, .gitignored)"
  git clone --recurse-submodules "$REPO" "$DEST"
fi
echo "done. Reference at ./$DEST (not part of this repo's MIT-licensed sources)."
