import { defineConfig, loadEnv } from 'vite';
import pkg from './package.json';
import react from '@vitejs/plugin-react';
export default defineConfig(({ mode }) => ({ plugins: [react()], define: { __APP_VERSION__: JSON.stringify(pkg.version) }, clearScreen: false, server: { port: 1420, strictPort: true, host: '127.0.0.1' }, build: { target: 'es2022', outDir: loadEnv(mode, '.', 'FUSION_UI_TEST_').FUSION_UI_TEST_DIST || 'dist' } }));
