#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const vendorDir = path.join(root, "vendor");
const vendorBinary = path.join(
  vendorDir,
  process.platform === "win32" ? "speak.exe" : "speak",
);
const releaseBinary = path.join(
  root,
  "target",
  "release",
  process.platform === "win32" ? "speak.exe" : "speak",
);

function run(label, command, args, env = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    env: { ...process.env, ...env },
  });
  if (result.status !== 0) {
    console.error(`FAIL ${label}`);
    if (result.stdout) {
      process.stdout.write(result.stdout);
    }
    if (result.stderr) {
      process.stderr.write(result.stderr);
    }
    process.exit(result.status ?? 1);
  }
  console.log(`ok ${label}`);
  return result;
}

run("syntax npm-install.js", process.execPath, ["--check", "scripts/npm-install.js"]);
run("syntax speak.js", process.execPath, ["--check", "bin/speak.js"]);
run("npm pack dry-run", "npm", ["pack", "--dry-run"]);

if (!fs.existsSync(releaseBinary)) {
  console.error(
    `missing ${releaseBinary}; run cargo build --release in the repo root first`,
  );
  process.exit(1);
}

fs.rmSync(vendorDir, { recursive: true, force: true });
fs.mkdirSync(vendorDir, { recursive: true });
fs.copyFileSync(releaseBinary, vendorBinary);
if (process.platform !== "win32") {
  fs.chmodSync(vendorBinary, 0o755);
}

const help = run("speak --help via bin", process.execPath, ["bin/speak.js", "--help"]);
if (!help.stdout.includes("list-voices")) {
  console.error("FAIL speak --help missing expected flags");
  process.exit(1);
}

run("speak --list-voices via bin", process.execPath, ["bin/speak.js", "--list-voices"]);

console.log("smoke-npm-local: all checks passed");