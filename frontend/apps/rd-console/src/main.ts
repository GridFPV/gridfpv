import { mount } from 'svelte';
// The component library's design tokens — imported once per surface so every
// @gridfpv/components widget themes off the same CSS custom properties.
import '@gridfpv/components/tokens.css';
// The console's global base (dark canvas, resets) — sits on top of the tokens.
import './app.css';
import App from './App.svelte';

const target = document.getElementById('app');
if (!target) {
  throw new Error('rd-console: #app mount target not found');
}

export default mount(App, { target });
