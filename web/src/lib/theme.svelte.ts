/**
 * Board colour theme, persisted per-browser in localStorage and applied as
 * `data-theme` on the document root. `Board.astro` sets the same attribute
 * inline before first paint so there's no flash of the wrong theme — this
 * module is what the settings picker reads and writes after hydration.
 */

export type ThemeId = 'dark' | 'light' | 'abyssal';

export const THEMES: Array<{ id: ThemeId; label: string }> = [
  { id: 'dark', label: 'Dark' },
  { id: 'light', label: 'Light' },
  { id: 'abyssal', label: 'Abyssal' },
];

const THEME_KEY = 'oarfish:theme';
const DEFAULT_THEME: ThemeId = 'dark';

let theme = $state(readStoredTheme());

function readStoredTheme(): ThemeId {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    return THEMES.some((t) => t.id === stored) ? (stored as ThemeId) : DEFAULT_THEME;
  } catch {
    return DEFAULT_THEME;
  }
}

/** The active theme id. */
export function currentTheme(): ThemeId {
  return theme;
}

/** Switch themes, persisting the choice and updating the document immediately. */
export function setTheme(id: ThemeId): void {
  theme = id;
  document.documentElement.dataset.theme = id;
  try {
    localStorage.setItem(THEME_KEY, id);
  } catch {
    // Private browsing or a blocked store: the theme still applies for this
    // page load, it just won't survive a reload.
  }
}
