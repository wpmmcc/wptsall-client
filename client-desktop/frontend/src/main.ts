import './app.css';
import App from './App.svelte';
import { mount } from 'svelte';
import { setApiFetchHandler } from '@webui/lib/api/client';
import { installGlobalErrorReporting } from '@webui/lib/errors/clientErrorReporting';
import { apiFetch as desktopApiFetch } from './lib/api/client';

setApiFetchHandler(desktopApiFetch);
// Route WebView JS crashes into the client backend log stream (the shared
// reporter goes through apiFetch, so it rides the Tauri bridge/proxy too).
installGlobalErrorReporting();

const app = mount(App, { target: document.getElementById('app')! });

export default app;
