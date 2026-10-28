"use strict";
const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { main, packageCommands, distroId, pulseConfig } = require("./setup.js");

// Real temporary files, mocked OS identity and process execution. No system writes.
function fixture(t, changes = {}) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "speak-setup-test-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const calls = [], logs = [];
  const map = file => file === "/etc/asound.conf" ? path.join(home, "system-asound.conf")
    : file === "/mnt/wslg" ? path.join(home, "wslg") : file;
  const host = {
    platform: "linux", release: "6.6-linux", uid: 1000, home, env: {}, tty: true,
    log: text => logs.push(text), confirm: async () => true,
    spawn: (program, args, options) => { calls.push({ program, args, options }); return { status: 0 }; },
    fs: { ...fs,
      readFileSync: (file, encoding) => file === "/etc/os-release" ? 'ID="ubuntu"\n' : fs.readFileSync(file, encoding),
      lstatSync: file => fs.lstatSync(map(file)),
    },
    ...changes,
  };
  return { host, calls, logs, home, config: path.join(home, ".asoundrc") };
}

function wsl(t, changes = {}) {
  const f = fixture(t, { release: "6.6-microsoft-standard-WSL2", env: { PULSE_SERVER: "unix:/mnt/wslg/PulseServer" }, ...changes });
  fs.mkdirSync(path.join(f.home, "wslg"));
  return f;
}

test("literal distro parser does not evaluate shell content or use ID_LIKE", () => {
  for (const value of ['ID=ubuntu', 'ID="ubuntu"', "ID='ubuntu'"]) assert.equal(distroId(value), "ubuntu");
  assert.equal(distroId('ID="$(touch /tmp/no)"\nID_LIKE=ubuntu'), "unknown");
  assert.equal(distroId('ID=debian;echo injected'), "unknown");
  assert.equal(distroId('ID_LIKE=ubuntu'), "unknown");
});

const expected = {
  debian: [["apt-get", "update"], ["apt-get", "install", "--no-install-recommends", "-y", "ca-certificates", "tar", "libasound2-plugins", "libstdc++6"]],
  ubuntu: [["apt-get", "update"], ["apt-get", "install", "--no-install-recommends", "-y", "ca-certificates", "tar", "libasound2-plugins", "libstdc++6"]],
  fedora: [["dnf", "install", "--setopt=install_weak_deps=False", "-y", "ca-certificates", "tar", "alsa-lib", "libstdc++"]],
  arch: [["pacman", "-S", "--needed", "--noconfirm", "ca-certificates", "tar", "alsa-lib", "alsa-plugins", "libpulse", "gcc-libs"]],
};
const buildPackages = {
  debian: ["build-essential", "pkg-config", "libasound2-dev", "libssl-dev"],
  ubuntu: ["build-essential", "pkg-config", "libasound2-dev", "libssl-dev"],
  fedora: ["gcc", "gcc-c++", "make", "pkgconf-pkg-config", "alsa-lib-devel", "openssl-devel"],
  arch: ["base-devel", "openssl"],
};
for (const id of Object.keys(expected)) {
  test(`${id}: exact runtime/build commands and actual execution arguments`, async t => {
    assert.deepEqual(packageCommands(id, false, true), expected[id]);
    const build = expected[id].map(command => [...command]);
    build.at(-1).push(...buildPackages[id]);
    assert.deepEqual(packageCommands(id, true, true), build);
    const f = fixture(t, { uid: 0 });
    f.host.fs.readFileSync = () => `ID=${id}\n`;
    await main(["--deps-only", "--runtime-only", "--yes"], f.host);
    assert.deepEqual(f.calls.map(c => [c.program, ...c.args]), expected[id].map(c =>
      c[0] === "apt-get" ? ["env", "DEBIAN_FRONTEND=noninteractive", ...c] : c));
    for (const call of f.calls) {
      assert.equal(call.options.shell, false);
      assert.deepEqual(call.options.stdio, ["ignore", "inherit", "inherit"]);
    }
  });
}

test("sudo is limited to package commands; binary install is unprivileged and last", async t => {
  const f = fixture(t);
  await main(["--yes"], f.host);
  assert.equal(f.calls.length, 3);
  assert.deepEqual(f.calls[0].args, ["-n", "--", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "update"]);
  assert.equal(f.calls[0].program, "sudo");
  assert.ok(f.calls[1].args.includes("libasound2-dev"));
  assert.equal(f.calls[2].program, process.execPath);
  assert.equal(path.basename(f.calls[2].args[0]), "npm-install.js");
});

test("interactive approval precedes all commands and allows sudo prompt", async t => {
  const f = fixture(t);
  f.host.confirm = async () => { assert.equal(f.calls.length, 0); assert.ok(f.logs.some(l => l.includes('"sudo"'))); return true; };
  await main(["--deps-only"], f.host);
  assert.deepEqual(f.calls[0].args, ["--", "apt-get", "update"]);
  assert.equal(f.calls[0].options.stdio, "inherit");
});

test("decline, noninteractive input, unknown options and unsupported distros have no side effects", async t => {
  for (const scenario of ["decline", "noninteractive", "option", "unsupported", "missing-release"]) {
    const f = wsl(t);
    const args = ["--deps-only"];
    if (scenario === "decline") f.host.confirm = async () => false;
    if (scenario === "noninteractive") f.host.tty = false;
    if (scenario === "option") args.push("--typo");
    if (scenario === "unsupported") f.host.fs.readFileSync = () => 'ID=alpine\nID_LIKE=debian';
    if (scenario === "missing-release") f.host.fs.readFileSync = () => { throw new Error("unreadable"); };
    await assert.rejects(main(args, f.host));
    assert.equal(f.calls.length, 0, scenario);
    assert.equal(fs.existsSync(f.config), false, scenario);
  }
});

test("dry-run prints all commands/config but never spawns, prompts or writes", async t => {
  const f = wsl(t, { tty: false, confirm: () => { throw new Error("prompted"); } });
  await main(["--dry-run"], f.host);
  assert.equal(f.calls.length, 0);
  assert.equal(fs.existsSync(f.config), false);
  assert.ok(f.logs.some(l => l.includes(pulseConfig)));
  assert.ok(f.logs.some(l => l.includes("npm-install.js")));
});

test("WSLg guidance never writes configs, including repeated runs", async t => {
  const f = wsl(t);
  await main(["--yes", "--deps-only"], f.host);
  assert.equal(fs.existsSync(f.config), false);
  assert.ok(f.logs.some(l => l.includes(pulseConfig)));
  fs.writeFileSync(f.config, "custom audio settings");
  await main(["--yes", "--deps-only"], f.host);
  assert.equal(fs.readFileSync(f.config, "utf8"), "custom audio settings");
});

test("native Linux gets no WSL advice; missing WSLg or PULSE_SERVER gets actionable advice", async t => {
  for (const kind of ["native", "missing-wslg", "missing-pulse"]) {
    const f = wsl(t);
    if (kind === "native") f.host.release = "linux";
    if (kind === "missing-wslg") fs.rmdirSync(path.join(f.home, "wslg"));
    if (kind === "missing-pulse") delete f.host.env.PULSE_SERVER;
    await main(["--yes", "--deps-only"], f.host);
    assert.equal(fs.existsSync(f.config), false, kind);
    assert.equal(f.logs.some(l => l.includes("enable WSL 2")), kind !== "native");
  }
});

test("Fedora gets PulseAudio default plugin only on WSLg with a plan warning", async t => {
  for (const enabled of [false, true]) {
    const f = enabled ? wsl(t) : fixture(t);
    f.host.fs.readFileSync = () => "ID=fedora\n";
    await main(["--yes", "--deps-only"], f.host);
    assert.equal(f.calls[0].args.includes("alsa-plugins-pulseaudio"), enabled);
    assert.equal(f.logs.some(l => l.includes("package-owned default")), enabled);
  }
});

test("package failures, missing sudo and signals stop before audio/binary work", async t => {
  for (const failure of [{ status: 100 }, { error: new Error("spawn sudo ENOENT") }, { status: null, signal: "SIGTERM" }]) {
    const f = wsl(t);
    f.host.spawn = (...args) => { f.calls.push(args); return failure; };
    await assert.rejects(main(["--yes"], f.host), /failed/);
    assert.equal(f.calls.length, 1);
    assert.equal(fs.existsSync(f.config), false);
  }
});

test("failed package install after update does not continue", async t => {
  const f = wsl(t);
  f.host.spawn = (...args) => { f.calls.push(args); return { status: f.calls.length === 2 ? 1 : 0 }; };
  await assert.rejects(main(["--yes"], f.host), /not rolled back/);
  assert.equal(f.calls.length, 2);
  assert.equal(fs.existsSync(f.config), false);
});

test("binary errors propagate", async t => {
  const f = fixture(t, { platform: "darwin" });
  f.host.spawn = () => ({ status: 2 });
  await assert.rejects(main(["--yes"], f.host), /exit 2/);
});

test("root cannot run downloads/builds; macOS/Windows deps-only are no-ops", async t => {
  const root = fixture(t, { uid: 0 });
  await assert.rejects(main(["--yes"], root.host), /normal user/);
  assert.equal(root.calls.length, 0);
  for (const platform of ["darwin", "win32"]) {
    const f = fixture(t, { platform, tty: false });
    await main(["--deps-only"], f.host);
    assert.equal(f.calls.length, 0);
  }
});

test("actual CLI reports option errors and help with correct status", () => {
  const script = path.join(__dirname, "setup.js");
  const bad = spawnSync(process.execPath, [script, "--bad"], { encoding: "utf8" });
  assert.equal(bad.status, 1);
  assert.match(bad.stderr, /Unknown option/);
  const good = spawnSync(process.execPath, [script, "--help"], { encoding: "utf8" });
  assert.equal(good.status, 0);
  assert.match(good.stderr, /--dry-run/);
});

test("npm fallback probes cargo directly without a shell", () => {
  const { commandExists } = require("./npm-install.js");
  const commands = [];
  assert.equal(commandExists("cargo", (...args) => { commands.push(args); return { status: 0 }; }), true);
  assert.deepEqual(commands, [["cargo", ["--version"], { stdio: "ignore", shell: false }]]);
  assert.equal(commandExists("cargo", () => ({ error: new Error("ENOENT"), status: null })), false);
  assert.equal(commandExists("cargo", () => ({ status: 1 })), false);
});

test("GitHub token is not forwarded to release-storage or untrusted origins", () => {
  const { downloadHeaders } = require("./npm-install.js");
  assert.equal(downloadHeaders("https://api.github.com/repos/o/r/releases", {}, "secret").Authorization, "Bearer secret");
  for (const url of ["https://release-assets.githubusercontent.com/file", "https://example.com", "https://api.github.com.evil.test", "http://api.github.com", "https://api.github.com:444"]) {
    assert.equal(downloadHeaders(url, { Accept: "application/octet-stream" }, "secret").Authorization, undefined);
  }
  assert.equal(downloadHeaders("https://api.github.com", {}, "").Authorization, undefined);
});
