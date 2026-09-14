import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  // The SDK page's samples are held to `agent-sdk-py/README.md` by test,
  // and that file is a directory above this package. Vite serves nothing
  // outside the project root unless told, so the repository root is
  // allowed too -- for the test's `?raw` import, and nothing else reads
  // up there.
  server: { fs: { allow: ['..'] } },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test-setup.ts'],
  },
})
