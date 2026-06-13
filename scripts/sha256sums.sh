#!/bin/sh
# Compute SHA256 checksums for all tarballs in artifacts/ directory
# Usage: scripts/sha256sums.sh [artifacts_dir]
# Output: SHA256SUMS file in the artifacts directory

set -e

ARTIFACTS="${1:-artifacts}"
SUMS_FILE="${ARTIFACTS}/SHA256SUMS"

if [ ! -d "$ARTIFACTS" ]; then
    echo "Usage: $0 [artifacts_dir]"
    echo "Directory '$ARTIFACTS' not found."
    exit 1
fi

echo "Computing SHA256 checksums for files in ${ARTIFACTS}..."

find "${ARTIFACTS}" -type f -name '*.tar.gz' -o -name '*.zip' | sort | while read -r file; do
    sha256sum "$file" >> "${SUMS_FILE}"
done

if [ -f "${SUMS_FILE}" ]; then
    echo "Checksums written to ${SUMS_FILE}"
    cat "${SUMS_FILE}"
else
    echo "No archive files found in ${ARTIFACTS}"
    exit 1
fi
