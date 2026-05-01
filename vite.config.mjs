import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Keep this config as ESM JavaScript so Vite can load it natively on Windows
// machines that block the default config bundling subprocess.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
})
