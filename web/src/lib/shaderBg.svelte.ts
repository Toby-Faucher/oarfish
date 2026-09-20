/**
 * Ocean shader background toggle, persisted per-browser in localStorage.
 * Off by default. Unlike theme.svelte.ts there's no inline pre-paint script
 * for this one — the default is "nothing renders," so there's no wrong
 * state to flash.
 */

const SHADER_BG_KEY = 'oarfish:shader-bg';

let enabled = $state(readStored());

function readStored(): boolean {
  try {
    return localStorage.getItem(SHADER_BG_KEY) === '1';
  } catch {
    return false;
  }
}

/** Whether the ocean background is turned on. */
export function shaderBgEnabled(): boolean {
  return enabled;
}

/** Turn the ocean background on or off, persisting the choice. */
export function setShaderBgEnabled(value: boolean): void {
  enabled = value;
  try {
    localStorage.setItem(SHADER_BG_KEY, value ? '1' : '0');
  } catch {
    // Private browsing or a blocked store: applies for this page load only.
  }
}
