import { defineConfig, devices } from '@playwright/test';
import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';

// Load .env.test if present (pure Node.js, no dotenv dependency)
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const envTestPath = path.join(__dirname, '.env.test');
if (fs.existsSync(envTestPath)) {
  for (const line of fs.readFileSync(envTestPath, 'utf-8').split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const eqIdx = trimmed.indexOf('=');
    if (eqIdx > 0) {
      const key = trimmed.slice(0, eqIdx).trim();
      const value = trimmed.slice(eqIdx + 1).trim();
      if (!process.env[key]) process.env[key] = value;
    }
  }
}

export default defineConfig({
  // Historical client-side real-system E2E reference suite.
  // Official cross-system gate lives under dev-tool/e2e/.
  testDir:        './tests/archived-cross-system',
  timeout:        420_000,   // 7 min - 3 relations × 53 items = 159 tasks with unlimited cap
  expect:         { timeout: 20_000 },
  fullyParallel:  false,
  retries:        0,
  reporter:       'list',
  use: {
    baseURL:             'http://127.0.0.1:8977',
    headless:            true,
    viewport:            { width: 1280, height: 800 },
    ignoreHTTPSErrors:   true,
    bypassCSP:           true,
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
  ],
});
