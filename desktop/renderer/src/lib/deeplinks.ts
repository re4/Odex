import { useApp } from '@/store/app'
import * as A from '@/lib/actions'
import { toast } from '@/lib/rpc'

/**
 * odex:// deep links:
 *   odex://threads/<id>           open a thread
 *   odex://new?prompt=…&project=… start a thread with a draft
 *   odex://settings/<panel>       open a settings panel
 *   odex://automations            open automations
 */
export async function handleDeepLink(raw: string): Promise<void> {
  let url: URL
  try {
    url = new URL(raw)
  } catch {
    return
  }
  if (url.protocol !== 'odex:') return
  const parts = `${url.host}${url.pathname}`.split('/').filter(Boolean)
  const s = useApp.getState()
  switch (parts[0]) {
    case 'threads':
    case 'thread':
      if (parts[1]) {
        if (!s.threads[parts[1]]) await s.refreshThreads().catch(() => {})
        if (useApp.getState().threads[parts[1]]) await s.selectThread(parts[1])
        else toast('That thread no longer exists', 'error')
      }
      break
    case 'new': {
      const project = url.searchParams.get('project')
      if (project) {
        const p = s.projects.find((x) => x.id === project || x.name === project || x.folders.includes(project))
        if (p) s.setUi({ newThreadProjectId: p.id })
      }
      await s.selectThread(null)
      const prompt = url.searchParams.get('prompt')
      // never auto-send from a link: put the text in the composer for review
      if (prompt) setTimeout(() => window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'text', text: prompt } })), 50)
      break
    }
    case 'settings':
      A.openSettings(parts[1] ?? 'general')
      break
    case 'automations':
      s.setUi({ view: 'automations' })
      break
    default:
      break
  }
}
