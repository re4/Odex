// `import.meta.glob` (Vite) for the settings search index; the project only loads Node types.
interface ImportMeta {
  glob(pattern: string | string[], options?: { query?: string; import?: string; eager?: boolean }): Record<string, () => Promise<unknown>>
}
