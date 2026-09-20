/**
 * Board-side settings, persisted per-browser in localStorage. Dev-only by
 * construction: `import.meta.env.DEV` is a build-time constant, so a
 * production build dead-code-eliminates everything here down to `false` —
 * the same safety property the demo alarm already had, now backing a
 * persistent toggle instead of a `?demo` URL param.
 */

const DEMO_MODE_KEY = 'oarfish:demo-mode';

let demoMode = $state(readStoredDemoMode());

function readStoredDemoMode(): boolean {
  if (!import.meta.env.DEV) return false;
  try {
    return localStorage.getItem(DEMO_MODE_KEY) === 'true';
  } catch {
    return false;
  }
}

/** Whether the dev-only demo alarm should render. Always `false` in production. */
export function demoModeEnabled(): boolean {
  return import.meta.env.DEV && demoMode;
}

/** Flip the demo alarm on or off, persisting the choice. No-op in production. */
export function setDemoMode(value: boolean): void {
  if (!import.meta.env.DEV) return;
  demoMode = value;
  try {
    localStorage.setItem(DEMO_MODE_KEY, String(value));
  } catch {
    // Private browsing or a blocked store: the toggle still works for this
    // page load, it just won't survive a reload.
  }
}
