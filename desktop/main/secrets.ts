import { safeStorage } from 'electron'
import fs from 'node:fs'
import path from 'node:path'
import { odexHome } from './paths'

/**
 * Secrets (endpoint API keys, GitHub token) encrypted with Electron safeStorage
 * (DPAPI on Windows, Keychain on macOS, libsecret on Linux) in ~/.odex/secrets.json.
 * Decrypted values are only handed to the engine process (in memory).
 */
type Store = Record<string, string>

function file(): string {
  return path.join(odexHome(), 'secrets.json')
}

function load(): Store {
  try {
    return JSON.parse(fs.readFileSync(file(), 'utf8')) as Store
  } catch {
    return {}
  }
}

function save(s: Store): void {
  fs.mkdirSync(path.dirname(file()), { recursive: true })
  fs.writeFileSync(file(), JSON.stringify(s, null, 2), { mode: 0o600 })
}

function enc(value: string): string {
  if (safeStorage.isEncryptionAvailable()) return 'enc:' + safeStorage.encryptString(value).toString('base64')
  // Without OS encryption (e.g. Linux without a keyring) we refuse plaintext.
  throw new Error('OS secret storage is unavailable; set the key via an environment variable instead (api_key_env).')
}

function dec(value: string): string | null {
  try {
    if (value.startsWith('enc:')) return safeStorage.decryptString(Buffer.from(value.slice(4), 'base64'))
  } catch {}
  return null
}

export function setSecret(key: string, value: string | null): void {
  const s = load()
  if (value == null || value === '') delete s[key]
  else s[key] = enc(value)
  save(s)
}

export function hasSecret(key: string): boolean {
  return key in load()
}

export function allSecrets(): Record<string, string> {
  const out: Record<string, string> = {}
  for (const [k, v] of Object.entries(load())) {
    const d = dec(v)
    if (d != null) out[k] = d
  }
  return out
}
