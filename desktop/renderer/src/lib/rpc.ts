import type { ClientMethod, ParamsOf, ResultOf } from '@shared/index'

/** Typed engine call: `await call('thread/list', {})`. */
export async function call<M extends ClientMethod>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>> {
  return (await window.odex.request(method, params)) as ResultOf<M>
}

/** Fire-and-forget call that reports failures through the toast system. */
export function callSafe<M extends ClientMethod>(method: M, params: ParamsOf<M>, onError?: (e: Error) => void): Promise<ResultOf<M> | undefined> {
  return call(method, params).catch((e: Error) => {
    if (onError) onError(e)
    else window.dispatchEvent(new CustomEvent('odex:toast', { detail: { kind: 'error', text: `${method}: ${e.message}` } }))
    return undefined
  })
}

export function toast(text: string, kind: 'info' | 'error' | 'success' = 'info'): void {
  window.dispatchEvent(new CustomEvent('odex:toast', { detail: { kind, text } }))
}
