/**
 * Deep settings search: an index of every setting's label and hint, built from
 * the settings panels' own source (`<Row label=… hint=…>`, section titles and
 * field labels), so new rows are searchable without a hand-kept keyword list.
 * The sources load lazily (a separate chunk) on the first search.
 *
 * Panel ids are derived from file names: `ComputerUseSettings.tsx` → `computer-use`
 * (compared without dashes, so `computeruse` matches too).
 */

export interface SettingHit {
  /** Settings panel id (as in the registry). */
  panel: string
  label: string
  hint?: string
}

const sources = import.meta.glob('./*Settings.tsx', { query: '?raw', import: 'default' }) as Record<string, () => Promise<string>>

const decode = (s: string): string =>
  s
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&apos;|&#39;/g, "'")
    .replace(/\s+/g, ' ')
    .trim()

/** Labels, hints and headings found in one panel's source. */
export function extractSettings(src: string): Array<{ label: string; hint?: string }> {
  const out: Array<{ label: string; hint?: string }> = []
  const seen = new Set<string>()
  const add = (label: string, hint?: string) => {
    const l = decode(label)
    if (!l || l.length > 80 || seen.has(l.toLowerCase())) return
    seen.add(l.toLowerCase())
    out.push({ label: l, hint: hint ? decode(hint) : undefined })
  }
  // <Row label="…" hint="…"> (attributes in any order, string literals only)
  for (const m of src.matchAll(/<Row\b([^>]*?)>/gs)) {
    const attrs = m[1]
    const label = /\blabel=(?:"([^"]+)"|\{'([^']+)'\}|\{"([^"]+)"\})/.exec(attrs)
    if (!label) continue
    const hint = /\bhint=(?:"([^"]+)"|\{'([^']+)'\}|\{"([^"]+)"\})/.exec(attrs)
    add(label[1] ?? label[2] ?? label[3], hint ? (hint[1] ?? hint[2] ?? hint[3]) : undefined)
  }
  // <Section title="…" desc="…"> and section headings
  for (const m of src.matchAll(/<Section\b[^>]*?\btitle="([^"]+)"(?:[^>]*?\bdesc="([^"]+)")?/gs)) add(m[1], m[2])
  for (const m of src.matchAll(/className="section-title"[^>]*>\s*([^<{]+?)\s*</g)) add(m[1])
  for (const m of src.matchAll(/<h[34]\b[^>]*>\s*([^<{]+?)\s*</g)) add(m[1])
  // form field labels
  for (const m of src.matchAll(/<label\b[^>]*>\s*([^<{]+?)\s*</g)) add(m[1])
  return out
}

let index: Promise<SettingHit[]> | null = null

/** Every indexed setting (loaded once). */
export function settingsIndex(): Promise<SettingHit[]> {
  index ??= (async () => {
    const all: SettingHit[] = []
    await Promise.all(
      Object.entries(sources).map(async ([file, load]) => {
        const name = /\/(\w+)Settings\.tsx$/.exec(file)?.[1]
        if (!name) return
        const panel = name.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()
        try {
          for (const s of extractSettings(await load())) all.push({ panel, ...s })
        } catch {
          /* a panel that fails to load just isn't indexed */
        }
      }),
    )
    return all
  })()
  return index
}

/** Normalize a panel id for matching file-derived ids against registry ids. */
export const panelKey = (id: string): string => id.replace(/-/g, '').toLowerCase()

/** Settings whose label or hint contains every word of `query`, best (label) matches first. */
export function searchSettings(all: SettingHit[], query: string): SettingHit[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean)
  if (!words.length) return []
  const scored: Array<{ hit: SettingHit; score: number }> = []
  for (const hit of all) {
    const label = hit.label.toLowerCase()
    const text = `${label} ${(hit.hint ?? '').toLowerCase()}`
    if (!words.every((w) => text.includes(w))) continue
    const score = (label.startsWith(words[0]) ? 4 : 0) + (words.every((w) => label.includes(w)) ? 2 : 0) - label.length / 200
    scored.push({ hit, score })
  }
  return scored.sort((a, b) => b.score - a.score).map((s) => s.hit)
}
