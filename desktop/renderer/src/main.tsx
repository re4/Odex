import { createRoot } from 'react-dom/client'
import 'highlight.js/styles/github.css'
import './styles/tokens.css'
import './styles/base.css'
import './styles/app.css'
import './styles/thread.css'
import { App } from './App'

createRoot(document.getElementById('root')!).render(<App />)
