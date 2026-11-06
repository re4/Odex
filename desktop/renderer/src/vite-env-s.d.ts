// `import.meta.glob` (Vite) for the settings search index; the project only loads Node types.
interface ImportMeta {
  glob(pattern: string | string[], options?: { query?: string; import?: string; eager?: boolean }): Record<string, () => Promise<unknown>>
}

// Side-effect stylesheet imports (Vite bundles them; TypeScript 6 checks that they resolve).
declare module '*.css' {}
