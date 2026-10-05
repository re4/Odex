import { useApp } from '@/store/app'
import * as A from '@/lib/actions'
import { toast } from '@/lib/rpc'

/**
 * odex:// deep links:
 *   odex://threads/<id>                         open a thread
 *   odex://threads/new?prompt=…&path=…&project=… new thread with a draft (never auto-sent)
 *   odex://new?prompt=…                          same as threads/new
 *   odex://settings/<panel>                     open a settings panel
 *   odex://skills                               skills settings
 *   odex://automations                          automations view
 */
export async function handleDeepLink(raw: string): Promise<void> {
  let url: URL
  try {
    url = new URL(raw)
  } catch {
    return
  }
  if (url.protocol !== 'odex:') return
  const parts = `${url.host}${url.pathname}`.split('/').filter(Boolean).map(decodeURIComponent)
  const s = useApp.getState()
  const newThread = async () => {
    const project = url.searchParams.get('project')
    const folder = url.searchParams.get('path')
    const norm = (p: string) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase()
    let p = project ? s.projects.find((x) => x.id === project || x.name === project) : undefined
    if (!p && folder) p = s.projects.find((x) => x.folders.some((f) => norm(f) === norm(folder)))
    if (!p && folder) {
      // unknown folder: offer to add it (goes through the trust prompt)
      if (await A.confirmDialog('Open folder', `Add ${folder} as a project?`, 'Add project')) {
        const id = await A.addProject([folder])
        if (id) s.setUi({ newThreadProjectId: id })
      }
    } else if (p) s.setUi({ newThreadProjectId: p.id })
    await useApp.getState().selectThread(null)
    const prompt = url.searchParams.get('prompt')
    // never auto-send from a link: put the text in the composer for review
    if (prompt) setTimeout(() => window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'text', text: prompt } })), 80)
  }
  switch (parts[0]) {
    case 'threads':
    case 'thread':
      if (parts[1] === 'new' || !parts[1]) {
        await newThread()
      } else {
        if (!s.threads[parts[1]]) await s.refreshThreads().catch(() => {})
        if (useApp.getState().threads[parts[1]]) await s.selectThread(parts[1])
        else toast('That thread no longer exists', 'error')
      }
      break
    case 'new':
      await newThread()
      break
    case 'settings':
      A.openSettings(parts[1] ?? 'general')
      break
    case 'skills':
      A.openSettings('skills')
      break
    case 'automations':
      s.setUi({ view: 'automations' })
      break
    default:
      toast(`Unknown link: ${raw}`, 'error')
      break
  }
}
