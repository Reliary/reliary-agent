#!/usr/bin/env bash
set -e

VERSION="${1:-0.4.1}"
TARBALL_URL="https://github.com/Reliary/reliary-agent/releases/download/v${VERSION}/reliary-v${VERSION}-x86_64-apple-darwin.tar.gz"

echo "Downloading ${TARBALL_URL} to compute SHA256..."
SHA256=$(curl -sL "${TARBALL_URL}" | shasum -a 256 | awk '{print $1}')

if [ -z "$SHA256" ]; then
    echo "Failed to compute SHA256. Does the release exist?"
    exit 1
fi

cat <<EOF > reliary-agent.rb
class ReliaryAgent < Formula
  desc "Grammar-free code intelligence daemon, CLI, MCP server, and API proxy"
  homepage "https://github.com/Reliary/reliary-agent"
  url "${TARBALL_URL}"
  sha256 "${SHA256}"
  version "${VERSION}"
  license "MIT"

  def install
    bin.install "reliary-agent"
  end

  test do
    system "#{bin}/reliary-agent", "--version"
  end
end
EOF

echo "Generated reliary-agent.rb"
