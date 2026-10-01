// Copies the installers Tauri buries in target/release/bundle/ into
// desktop/release/, with the names they ship under. Used by `npm run pack` and by
// the release workflows, so both produce exactly the same file names.
//
//   Windows → bundle/nsis/*-setup.exe  → Coucou-Windows-<version>-setup.exe (+ rolling name)
//   Linux   → bundle/{deb,rpm,appimage} → Coucou-Linux-<version>-<arch>.<ext>

import { readFileSync, mkdirSync, copyFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { dirname, join, resolve, extname } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const bundleRoot = join(root, "target", "release", "bundle");
const outDir = join(root, "release");

const { version } = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));

const mb = (file) => (statSync(file).size / 1024 / 1024).toFixed(2);

/** Newest file in `dir` whose name satisfies `match`, or null. */
function newest(dir, match) {
  if (!existsSync(dir)) return null;
  const files = readdirSync(dir).filter(match).map((f) => join(dir, f));
  files.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return files[0] ?? null;
}

function fail(dir) {
  console.error(`No installer in ${dir} — run \`npm run tauri build\` first.`);
  process.exit(1);
}

mkdirSync(outDir, { recursive: true });

if (process.platform === "win32") {
  const bundleDir = join(bundleRoot, "nsis");
  // Newest wins, in case an older build is still lying around.
  const built = newest(bundleDir, (f) => f.endsWith("-setup.exe"));
  if (!built) fail(bundleDir);

  const versioned = join(outDir, `Coucou-Windows-${version}-setup.exe`);
  const rolling = join(outDir, "Coucou-Windows-setup.exe");
  copyFileSync(built, versioned);
  copyFileSync(built, rolling);

  console.log(`\n  Installer ready — ${mb(versioned)} MB\n`);
  console.log(`  ${versioned}`);
  console.log(`  ${rolling}\n`);
} else {
  // The architecture word differs per format (amd64 / x86_64 / aarch64); keep the
  // one Tauri chose so the names stay recognisable to each package manager.
  const kinds = [
    ["deb", ".deb"],
    ["rpm", ".rpm"],
    ["appimage", ".AppImage"],
  ];
  const copied = [];
  for (const [dir, ext] of kinds) {
    const built = newest(join(bundleRoot, dir), (f) => extname(f) === ext);
    if (!built) continue;
    const name = built.split("/").pop();
    const arch = name.match(/(amd64|x86_64|aarch64|arm64|armhf|armv7)/)?.[1] ?? "unknown";
    const target = join(outDir, `Coucou-Linux-${version}-${arch}${ext}`);
    copyFileSync(built, target);
    copied.push(target);
  }
  if (copied.length === 0) fail(bundleRoot);
  console.log("\n  Packages ready:\n");
  for (const f of copied) console.log(`  ${f}  (${mb(f)} MB)`);
  console.log("");
}
