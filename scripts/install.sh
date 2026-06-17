#!/bin/sh
set -e

RELIARY_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
PI_DIR="${HOME}/.pi/agent"

echo "Installing reliary-agent..."

# Install binary
mkdir -p "${BIN_DIR}"
cp "${RELIARY_DIR}/bin/reliary-agent" "${BIN_DIR}/reliary-agent"
chmod +x "${BIN_DIR}/reliary-agent"
echo "  ✓ binary → ${BIN_DIR}/reliary-agent"

# Create 'rel' symlink for convenience
ln -sf "${BIN_DIR}/reliary-agent" "${BIN_DIR}/rel"
echo "  ✓ symlink → ${BIN_DIR}/rel"

# Register Pi extension
if command -v pi >/dev/null 2>&1; then
  pi install "${RELIARY_DIR}/pi/gate.js" 2>/dev/null || {
    # Manual registration fallback
    mkdir -p "${PI_DIR}"
    if [ -f "${PI_DIR}/settings.json" ]; then
      python3 -c "
import json
with open('${PI_DIR}/settings.json') as f:
    cfg = json.load(f)
ext = '${RELIARY_DIR}/pi/gate.js'
if ext not in cfg.get('packages', []):
    cfg.setdefault('packages', []).append(ext)
    cfg.setdefault('extensions', []).append(ext)
    with open('${PI_DIR}/settings.json', 'w') as f:
        json.dump(cfg, f, indent=2)
print('  ✓ gate.js registered manually')
"
    fi
  }
  echo "  ✓ Pi extension registered"
else
  echo "  ⚠ pi not found — gate.js at ${RELIARY_DIR}/pi/gate.js (install manually: pi install ${RELIARY_DIR}/pi/gate.js)"
fi

# Add to PATH if not present
case ":${PATH}:" in
  *:"${BIN_DIR}":*) ;;
  *) echo "  ⚠ ${BIN_DIR} not in PATH. Add: export PATH=\"${BIN_DIR}:\$PATH\"" ;;
esac

echo ""
echo "reliary-agent installed successfully."
echo "  Run: reliary-agent serve"
echo "  Or:  rel --help"