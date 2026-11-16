import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

process.env.TZ = 'UTC';

export default defineConfig({
  plugins: [react()],
  server: { proxy: { '/api': 'http://127.0.0.1:8787' } },
  test: { environment: 'jsdom', setupFiles: ['./src/test-setup.ts'] },
});
