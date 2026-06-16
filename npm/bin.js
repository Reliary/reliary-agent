#!/usr/bin/env node
const { spawnSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');

const binName = os.platform() === 'win32' ? 'reliary-agent.exe' : 'reliary-agent';
const binPath = path.join(__dirname, 'bin', binName);

if (!fs.existsSync(binPath)) {
  console.error(`Error: reliary-agent binary not found at ${binPath}`);
  console.error('Please run "npm install" or "npm rebuild" to download it.');
  process.exit(1);
}

const args = process.argv.slice(2);
const result = spawnSync(binPath, args, { stdio: 'inherit' });

process.exit(result.status ?? 1);
