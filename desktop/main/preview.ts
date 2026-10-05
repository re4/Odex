import { app, BrowserWindow, dialog, ipcMain, protocol } from 'electron'
import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'

/*
 * Live HTML previews for the Files panel. The renderer's CSP (inherited by
 * srcdoc/blob:/data: frames) blocks inline scripts, so previews are served
 * from a private `odex-preview://<token>/<file>` scheme instead: the page
 * gets its own (empty) CSP, runs in a sandboxed iframe without same-origin,
 * and relative assets resolve inside the HTML file's folder (never outside it).
 */

export const PREVIEW_SCHEME = 'odex-preview'

/** token → folder served under it */
const roots = new Map<string, string>()
const tokens = new Map<string, string>()

const MIME: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.htm': 'text/html; charset=utf-8',
  '.xhtml': 'application/xhtml+xml; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.map': 'application/json; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
  '.md': 'text/plain; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.avif': 'image/avif',
  '.ico': 'image/x-icon',
  '.bmp': 'image/bmp',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.otf': 'font/otf',
  '.wasm': 'application/wasm',
  '.mp4': 'video/mp4',
  '.webm': 'video/webm',
  '.mp3': 'audio/mpeg',
  '.wav': 'audio/wav',
}

/** Must run before the app is ready. */
export function registerPreviewScheme(): void {
  protocol.registerSchemesAsPrivileged([{ scheme: PREVIEW_SCHEME, privileges: { standard: true, secure: true, supportFetchAPI: true, stream: true } }])
}

function tokenFor(dir: string): string {
  const key = process.platform === 'win32' ? dir.toLowerCase() : dir
  let t = tokens.get(key)
  if (!t) {
    t = crypto.randomBytes(12).toString('hex')
    tokens.set(key, t)
    roots.set(t, dir)
    // keep the table small
    if (tokens.size > 200) {
      const [oldKey, oldToken] = tokens.entries().next().value as [string, string]
      tokens.delete(oldKey)
      roots.delete(oldToken)
    }
  }
  return t
}

const notFound = () => new Response('Not found', { status: 404, headers: { 'content-type': 'text/plain' } })

/** Serve preview files (call once the app is ready). */
export function handlePreviewProtocol(): void {
  protocol.handle(PREVIEW_SCHEME, async (req) => {
    let u: URL
    try {
      u = new URL(req.url)
    } catch {
      return notFound()
    }
    const root = roots.get(u.hostname)
    if (!root) return notFound()
    const rel = decodeURIComponent(u.pathname).replace(/^[/\\]+/, '')
    const file = path.resolve(root, rel)
    const inside = path.relative(root, file)
    if (!rel || inside.startsWith('..') || path.isAbsolute(inside)) return new Response('Forbidden', { status: 403 })
    try {
      const st = await fs.promises.stat(file)
      if (!st.isFile() || st.size > 50 * 1024 * 1024) return notFound()
      const body = await fs.promises.readFile(file)
      return new Response(body, {
        headers: {
          'content-type': MIME[path.extname(file).toLowerCase()] ?? 'application/octet-stream',
          'cache-control': 'no-store',
          // module scripts from the opaque-origin (sandboxed) page need CORS
          'access-control-allow-origin': '*',
        },
      })
    } catch {
      return notFound()
    }
  })
}

/** IPC: preview URLs and "Save as…" copies of files on disk. */
export function registerPreviewIpc(): void {
  ipcMain.handle('preview:url', (_e, file: string) => {
    const abs = path.resolve(file)
    return `${PREVIEW_SCHEME}://${tokenFor(path.dirname(abs))}/${encodeURIComponent(path.basename(abs))}`
  })
  // copy an existing file where the user picks (the save dialog is the approval)
  ipcMain.handle('dialog:saveCopy', async (e, src: string, opts?: { title?: string }) => {
    const w = BrowserWindow.fromWebContents(e.sender)
    const ext = path.extname(src).slice(1)
    const o: Electron.SaveDialogOptions = {
      title: opts?.title ?? `Save a copy of ${path.basename(src)}`,
      defaultPath: path.join(app.getPath('downloads'), path.basename(src)),
      filters: ext ? [{ name: ext.toUpperCase(), extensions: [ext] }, { name: 'All files', extensions: ['*'] }] : undefined,
      properties: ['createDirectory', 'showOverwriteConfirmation'],
    }
    const r = w ? await dialog.showSaveDialog(w, o) : await dialog.showSaveDialog(o)
    if (r.canceled || !r.filePath) return null
    if (path.resolve(src) !== path.resolve(r.filePath)) {
      await fs.promises.mkdir(path.dirname(r.filePath), { recursive: true })
      await fs.promises.copyFile(src, r.filePath)
    }
    return r.filePath
  })
}
