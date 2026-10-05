import { resolve } from 'node:path'
import { defineConfig, externalizeDepsPlugin } from 'electron-vite'
import react from '@vitejs/plugin-react'

const shared = resolve(__dirname, 'shared-types/src')
// ODEX_OUT lets parallel builds (e.g. several e2e runs) use separate output dirs.
const out = process.env.ODEX_OUT || 'out'

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin()],
    resolve: { alias: { '@shared': shared } },
    build: { outDir: `${out}/main`, lib: { entry: resolve(__dirname, 'main/index.ts') } },
  },
  preload: {
    plugins: [externalizeDepsPlugin()],
    resolve: { alias: { '@shared': shared } },
    build: {
      outDir: `${out}/preload`,
      lib: { entry: resolve(__dirname, 'preload/index.ts') },
      rollupOptions: { output: { format: 'cjs', entryFileNames: '[name].cjs' } },
    },
  },
  renderer: {
    root: resolve(__dirname, 'renderer'),
    resolve: { alias: { '@shared': shared, '@': resolve(__dirname, 'renderer/src') } },
    plugins: [react()],
    build: { outDir: resolve(__dirname, out, 'renderer'), rollupOptions: { input: resolve(__dirname, 'renderer/index.html') } },
  },
})
