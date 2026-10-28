#!/usr/bin/env node
"use strict";

// Explicit user entry point. Never called by npm's automatic install lifecycle.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const readline = require("node:readline/promises");

const help = `Usage: node scripts/setup.js [options]
Install Linux runtime and source-build dependencies, then the speak binary.
Requires Node 18+. Install Rust separately if a source build is needed.
  --deps-only       Only install system dependencies
  --runtime-only    Omit compiler, headers and build tools (prebuilt binary)
  --dry-run         Print the complete plan without executing or writing
  --yes             Accept the plan; never prompt (sudo -n)
  --help            Show this help
Run as your normal user; only package-manager commands use sudo.
`;

const pulseConfig = "pcm.!default { type pulse }\nctl.!default { type pulse }\n";

function options(args) {
  const allowed = new Set(["--deps-only", "--runtime-only", "--dry-run", "--yes", "--help"]);
  for (const arg of args) {
    if (!allowed.has(arg)) throw new Error(`Unknown option: ${JSON.stringify(arg)}. Use --help.`);
  }
  return new Set(args);
}

// Parse only the one literal field we need. Never source/eval os-release.
function distroId(contents) {
  const match = contents.match(/^ID=(?:"([a-z0-9._-]+)"|'([a-z0-9._-]+)'|([a-z0-9._-]+))\s*$/m);
  return match ? match[1] || match[2] || match[3] : "unknown";
}

function packageCommands(id, build, yes, wslg = false) {
  const common = ["ca-certificates", "tar"];
  switch (id) {
    case "debian":
    case "ubuntu":
      return [
        ["apt-get", "update"],
        ["apt-get", "install", "--no-install-recommends", ...(yes ? ["-y"] : []),
          ...common, "libasound2-plugins", "libstdc++6",
          ...(build ? ["build-essential", "pkg-config", "libasound2-dev", "libssl-dev"] : [])],
      ];
    case "fedora":
      return [["dnf", "install", "--setopt=install_weak_deps=False", ...(yes ? ["-y"] : []),
        ...common, "alsa-lib", ...(wslg ? ["alsa-plugins-pulseaudio"] : []), "libstdc++",
        ...(build ? ["gcc", "gcc-c++", "make", "pkgconf-pkg-config", "alsa-lib-devel", "openssl-devel"] : [])]];
    case "arch":
      // Do not refresh only the databases (-Sy): Arch does not support partial upgrades.
      return [["pacman", "-S", "--needed", ...(yes ? ["--noconfirm"] : []),
        ...common, "alsa-lib", "alsa-plugins", "libpulse", "gcc-libs",
        ...(build ? ["base-devel", "openssl"] : [])]];
    default:
      throw new Error(`Unsupported Linux distro ID ${JSON.stringify(id)}. No changes made. ` +
        "Install ALSA (including its PulseAudio plugin), the C++ runtime, CA certificates and tar manually; " +
        "source builds also need Rust, a C/C++ compiler, make, pkg-config, ALSA and OpenSSL headers. " +
        "Then use npm install or cargo install directly. See README for the supported matrix.");
  }
}

function exists(file, io) {
  try {
    io.lstatSync(file);
    return true;
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

function isWsl(host) {
  return host.platform === "linux" && (/microsoft/i.test(host.release) || !!host.env.WSL_DISTRO_NAME);
}

function isWslg(host) {
  return isWsl(host) && !!host.env.PULSE_SERVER && exists("/mnt/wslg", host.fs);
}

function audioGuidance(host, wslg) {
  if (!isWsl(host)) return;
  if (!wslg) {
    host.log("WSL without WSLg/PULSE_SERVER: enable WSL 2 + WSLg for --play. No audio daemon is installed or started by setup.");
    return;
  }
  host.log("WSLg: use the existing PULSE_SERVER. Setup never writes ALSA configuration. " +
    "If --play cannot find a default device, review your existing ALSA settings and merge this default only if needed (see README):\n" + pulseConfig);
}

async function confirm() {
  const prompt = readline.createInterface({ input: process.stdin, output: process.stderr });
  try {
    return /^y(es)?$/i.test((await prompt.question("Apply this plan? [y/N] ")).trim());
  } finally {
    prompt.close();
  }
}

function run(command, host, yes) {
  const [program, ...args] = command;
  const result = host.spawn(program, args, {
    stdio: yes ? ["ignore", "inherit", "inherit"] : "inherit",
    env: host.env,
    shell: false,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`${program} failed: ${result.error?.message || result.signal || `exit ${result.status}`}. ` +
      "Stopped; earlier successful package operations are not rolled back. " +
      "Check the error above (including sudo access or package repositories), then rerun setup.");
  }
}

async function main(args = process.argv.slice(2), overrides = {}) {
  const flags = options(args);
  const host = {
    platform: process.platform, release: os.release(), uid: process.getuid?.(),
    env: process.env, fs, spawn: spawnSync,
    tty: !!process.stdin.isTTY, log: console.error, confirm,
    ...overrides,
  };
  if (flags.has("--help")) { host.log(help); return; }
  const yes = flags.has("--yes");
  const build = !flags.has("--runtime-only");
  let commands = [];
  const wslg = isWslg(host);
  if (host.platform === "linux") {
    let release;
    try { release = host.fs.readFileSync("/etc/os-release", "utf8"); }
    catch (error) { throw new Error(`Cannot read /etc/os-release: ${error.message}. Install dependencies manually; see README.`); }
    const id = distroId(release);
    commands = packageCommands(id, build, yes, wslg).map(command => {
      if (yes && command[0] === "apt-get") command = ["env", "DEBIAN_FRONTEND=noninteractive", ...command];
      return host.uid === 0 ? command : ["sudo", ...(yes ? ["-n"] : []), "--", ...command];
    });
    if (id === "fedora" && wslg) host.log("Fedora's PulseAudio plugin adds a package-owned default ALSA route. Existing .asoundrc and /etc/asound.conf stay untouched; review custom conf.d routing before accepting.");
    if (id === "arch") host.log("Arch: keep the system fully upgraded before setup (pacman -Syu); setup does not refresh databases or upgrade the whole system.");
  } else if (host.platform === "darwin" || host.platform === "win32") {
    host.log("Native OS audio needs no Linux packages. Source builds require Rust and Xcode Command Line Tools (macOS) or MSVC Build Tools (Windows).");
  } else {
    throw new Error(`Unsupported platform ${host.platform}. Install dependencies manually; see README.`);
  }

  audioGuidance(host, wslg);
  if (build) host.log("Source-build system dependencies included; Rust/cargo must already be installed if the binary download falls back to a build.");
  if (!flags.has("--deps-only")) {
    if (host.uid === 0 || host.env.SUDO_USER) {
      throw new Error("Run setup as your normal user so binary downloads/builds are not elevated. Root may use --deps-only.");
    }
    commands.push([process.execPath, path.join(__dirname, "npm-install.js")]);
  }
  host.log("Planned commands (JSON argument arrays; executed without a shell):");
  for (const command of commands) host.log(JSON.stringify(command));
  if (flags.has("--dry-run")) return;
  if (commands.length === 0) return;
  if (!yes && !host.tty) throw new Error("Noninteractive input: use --yes to accept the plan, or --dry-run to inspect it. No changes made.");
  if (!yes && !await host.confirm()) throw new Error("Cancelled. No changes made.");

  // Stop on the first failure, before attempting a binary download/build.
  for (const command of commands) run(command, host, yes);
  host.log("Setup complete. Package availability does not verify an audio device; test with speak --play.");
}

if (require.main === module) {
  main().catch(error => { console.error(`speak setup: ${error.message}`); process.exitCode = 1; });
}
module.exports = { main, packageCommands, distroId, pulseConfig };
