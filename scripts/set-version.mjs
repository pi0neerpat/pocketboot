import { readFile, writeFile } from "node:fs/promises";

const version = process.argv[2];
if (!version || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
  console.error("Usage: node scripts/set-version.mjs <major.minor.patch>");
  process.exit(2);
}

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

async function writeJson(path, value) {
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

const packageJson = await readJson("package.json");
const current = packageJson.version.split(".").map(Number);
const requested = version.split(".").map(Number);
const isOlder = requested.some((part, index) =>
  part < current[index] && requested.slice(0, index).every((prefix, i) => prefix === current[i]),
);
if (isOlder) {
  throw new Error(`Version ${version} is older than the current version ${packageJson.version}`);
}
const tauriConfig = await readJson("src-tauri/tauri.conf.json");
packageJson.version = version;
tauriConfig.version = version;
await writeJson("package.json", packageJson);
await writeJson("src-tauri/tauri.conf.json", tauriConfig);

const cargoTomlPath = "src-tauri/Cargo.toml";
let cargoToml = await readFile(cargoTomlPath, "utf8");
const packageVersion = /(\[package\][^\[]*?\nversion = ")[^"\n]+(")/;
if (!packageVersion.test(cargoToml)) {
  throw new Error(`Could not find the [package] version in ${cargoTomlPath}`);
}
cargoToml = cargoToml.replace(packageVersion, `$1${version}$2`);
await writeFile(cargoTomlPath, cargoToml);

const cargoLockPath = "src-tauri/Cargo.lock";
let cargoLock = await readFile(cargoLockPath, "utf8");
const lockVersion = /(\[\[package\]\]\nname = "pocketboot"\nversion = ")[^"\n]+(")/g;
const matches = [...cargoLock.matchAll(lockVersion)];
if (matches.length !== 1) {
  throw new Error(`Expected one pocketboot package entry in ${cargoLockPath}, found ${matches.length}`);
}
cargoLock = cargoLock.replace(lockVersion, `$1${version}$2`);
await writeFile(cargoLockPath, cargoLock);

console.log(`Updated PocketBoot version to ${version}`);
