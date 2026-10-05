import type { OdexApi } from '../../preload/index'

declare global {
  interface Window {
    odex: OdexApi
  }
}

export {}
