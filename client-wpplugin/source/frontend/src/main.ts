import './lib/i18n';
import './app.css';
import App from './App.svelte';
import { mount } from 'svelte';
import { installGlobalErrorReporting } from './lib/errors/clientErrorReporting';

// Surface JS crashes (window.onerror / unhandledrejection) in the client
// backend's log stream — installed before the app mounts so boot-time
// errors are captured too.
installGlobalErrorReporting();

const app = mount(App, { target: document.getElementById('app')! });
export default app;
