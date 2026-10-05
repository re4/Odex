#!/usr/bin/env node
// Stage the release `odex-engine` binary into desktop/build/bin/ so electron-builder can ship it
// as an extra resource (the packaged app looks for it at process.resourcesPath/bin, see main/paths.ts).
//
// Usage: node scripts/prepare-engine.mjs [--build]
//   --build             run `cargo build --release -p odex-engine` first
//   ODEX_ENGINE_BIN     explicit path to the binary to stage (skips the lookup below)
//   CARGO_TARGET_DIR    cargo target dir to look in (relative paths resolve against engine/)
//
// Lookup order: $ODEX_ENGINE_BIN, $CARGO_TARGET_DIR/release, engine/target/release.
// Presets (presets/models.toml) are embedded in the binary with include_str!, so nothing else is staged.

/* global process, console */

import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const desktopDir = path.resolve(here, '..')
const engineDir = path.resolve(desktopDir, '..', 'engine')
const exe = process.platform === 'win32' ? '.exe' : ''
const name = `odex-engine${exe}`
const outDir = path.join(desktopDir, 'build', 'bin')

function fail(msg) {
  console.error(`prepare-engine: ${msg}`)
  process.exit(1)
}

function candidates() {
  const list = []
  if (process.env.ODEX_ENGINE_BIN) list.push(path.resolve(process.env.ODEX_ENGINE_BIN))
  if (process.env.CARGO_TARGET_DIR) list.push(path.resolve(engineDir, process.env.CARGO_TARGET_DIR, 'release', name))
  list.push(path.join(engineDir, 'target', 'release', name))
  return list
}

/** Newest mtime of any engine source file, to warn about a stale binary. */
function newestSource(dir, depth = 0) {
  let newest = 0
  let entries = []
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true })
  } catch {
    return 0
  }
  for (const e of entries) {
    if (e.name.startsWith('target') || e.name.startsWith('.') || e.name === 'node_modules') continue
    const p = path.join(dir, e.name)
    if (e.isDirectory() && depth < 6) newest = Math.max(newest, newestSource(p, depth + 1))
    else if (e.isFile() && (e.name.endsWith('.rs') || e.name === 'Cargo.toml' || e.name === 'Cargo.lock')) {
      newest = Math.max(newest, fs.statSync(p).mtimeMs)
    }
  }
  return newest
}

if (process.argv.includes('--build')) {
  console.log('prepare-engine: cargo build --release -p odex-engine')
  const r = spawnSync('cargo', ['build', '--release', '-p', 'odex-engine'], { cwd: engineDir, stdio: 'inherit' })
  if (r.status !== 0) fail(`cargo build failed (exit ${r.status ?? r.error?.message})`)
}

const src = candidates().find((p) => fs.existsSync(p))
if (!src) {
  fail(
    `no release engine binary found. Looked in:\n  ${candidates().join('\n  ')}\n` +
      'Build it with `cargo build --release -p odex-engine` in engine/ (or pass --build).',
  )
}

const stat = fs.statSync(src)
if (newestSource(engineDir) > stat.mtimeMs) {
  console.warn(`prepare-engine: warning: ${src} is older than the engine sources; rebuild it to ship current code.`)
}

fs.rmSync(outDir, { recursive: true, force: true })
fs.mkdirSync(outDir, { recursive: true })
const dest = path.join(outDir, name)
fs.copyFileSync(src, dest)
if (process.platform !== 'win32') fs.chmodSync(dest, 0o755)

const v = spawnSync(dest, ['--version'], { encoding: 'utf8' })
const version = v.status === 0 ? v.stdout.trim() : 'version check failed'
console.log(`prepare-engine: ${src} -> ${path.relative(desktopDir, dest)} (${(stat.size / 1048576).toFixed(1)} MB, ${version})`)
