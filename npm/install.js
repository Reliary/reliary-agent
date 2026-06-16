const os = require('os');
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const version = require('./package.json').version;
const repo = 'Reliary/reliary-agent';

const PLATFORMS = {
  win32: 'pc-windows-msvc',
  darwin: 'apple-darwin',
  linux: 'unknown-linux-gnu'
};

const ARCHITECTURES = {
  x64: 'x86_64',
  arm64: 'aarch64'
};

const platform = PLATFORMS[os.platform()];
const arch = ARCHITECTURES[os.arch()];

if (!platform || !arch) {
  console.error(`Unsupported platform: ${os.platform()} ${os.arch()}`);
  process.exit(1);
}

const target = `${arch}-${platform}`;
// release.yml creates reliary-v0.4.1-x86_64-unknown-linux-gnu.tar.gz
const filename = `reliary-v${version}-${target}.tar.gz`;
const url = `https://github.com/${repo}/releases/download/v${version}/${filename}`;

const binDir = path.join(__dirname, 'bin');

async function downloadAndExtract() {
  if (!fs.existsSync(binDir)) {
    fs.mkdirSync(binDir, { recursive: true });
  }

  console.log(`Downloading reliary-agent v${version} for ${target}...`);
  console.log(`URL: ${url}`);
  
  const tmpFile = path.join(__dirname, filename);
  
  try {
    if (os.platform() === 'win32') {
      execSync(`curl -sSfL -o "${tmpFile}" "${url}"`, { stdio: 'inherit' });
      // Windows tar doesn't reliably support --strip-components
      execSync(`tar -xf "${tmpFile}" -C "${binDir}"`, { stdio: 'inherit' });
      // Move the binary out of the extracted folder
      const extractedFolder = path.join(binDir, `reliary-v${version}-${target}`);
      const binSrc = path.join(extractedFolder, 'reliary-agent.exe');
      const binDst = path.join(binDir, 'reliary-agent.exe');
      if (fs.existsSync(binSrc)) {
        fs.renameSync(binSrc, binDst);
      }
      fs.rmdirSync(extractedFolder, { recursive: true });
    } else {
      execSync(`curl -sSfL "${url}" | tar xz -C "${binDir}" --strip-components=1`, { stdio: 'inherit' });
    }
    
    // On Unix, ensure the binary is executable
    if (os.platform() !== 'win32') {
      const binPath = path.join(binDir, 'reliary-agent');
      if (fs.existsSync(binPath)) {
        fs.chmodSync(binPath, 0o755);
      }
    }
    
    console.log('Successfully installed reliary-agent.');
  } catch (error) {
    console.error('Failed to download and extract reliary-agent binary.', error);
    process.exit(1);
  } finally {
    if (fs.existsSync(tmpFile)) {
      fs.unlinkSync(tmpFile);
    }
  }
}

downloadAndExtract();
