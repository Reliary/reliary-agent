#!/usr/bin/env bash
# Generate Homebrew formula for reliary-agent with multi-arch support.
# Usage: generate_brew_formula.sh VERSION [OUTPUT_DIR]
#   VERSION    - release version (e.g., 0.6.3)
#   OUTPUT_DIR - where to write reliary-agent.rb (default: current dir)
set -e

VERSION="${1#v}"
VERSION="${VERSION:-0.6.3}"
OUTPUT_DIR="${2:-.}"
BASE_URL="https://github.com/Reliary/reliary-agent/releases/download/v${VERSION}"

declare -A TARBALLS
TARBALLS[x86_64-darwin]="reliary-v${VERSION}-x86_64-apple-darwin.tar.gz"
TARBALLS[aarch64-darwin]="reliary-v${VERSION}-aarch64-apple-darwin.tar.gz"
TARBALLS[x86_64-linux]="reliary-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
TARBALLS[aarch64-linux]="reliary-v${VERSION}-aarch64-unknown-linux-gnu.tar.gz"

declare -A SHAS

for key in "${!TARBALLS[@]}"; do
    url="${BASE_URL}/${TARBALLS[$key]}"
    echo "Downloading ${TARBALLS[$key]} to compute SHA256..."
    sha=$(curl -sL "$url" | shasum -a 256 | awk '{print $1}')
    if [ -z "$sha" ]; then
        echo "FAILED: could not compute SHA for ${TARBALLS[$key]}"
        exit 1
    fi
    SHAS[$key]="$sha"
    echo "  SHA256: $sha"
done

cat > "${OUTPUT_DIR}/reliary-agent.rb" << RUBY
class ReliaryAgent < Formula
  desc "API proxy, code search, and edit safety for coding agents"
  homepage "https://github.com/Reliary/reliary-agent"
  license "MIT"
  version "${VERSION}"

  on_macos do
    if Hardware::CPU.arm?
      url "${BASE_URL}/${TARBALLS[aarch64-darwin]}"
      sha256 "${SHAS[aarch64-darwin]}"
    else
      url "${BASE_URL}/${TARBALLS[x86_64-darwin]}"
      sha256 "${SHAS[x86_64-darwin]}"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "${BASE_URL}/${TARBALLS[aarch64-linux]}"
      sha256 "${SHAS[aarch64-linux]}"
    else
      url "${BASE_URL}/${TARBALLS[x86_64-linux]}"
      sha256 "${SHAS[x86_64-linux]}"
    end
  end

  def install
    bin.install "reliary-agent"
  end

  test do
    system "\#{bin}/reliary-agent", "--version"
  end
end
RUBY

echo "Generated ${OUTPUT_DIR}/reliary-agent.rb"
