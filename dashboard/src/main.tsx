import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import './index.css'
import App from './App.tsx'
import ErrorBoundary, { BrokenPage } from './components/ErrorBoundary'

// The boundary sits inside the router so a page that throws is replaced
// by a page, not by nothing -- see `ErrorBoundary` for the two times
// this app has rendered a blank white document instead.
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <BrowserRouter>
      <ErrorBoundary fallback={(error) => <BrokenPage error={error} />}>
        <App />
      </ErrorBoundary>
    </BrowserRouter>
  </StrictMode>,
)
