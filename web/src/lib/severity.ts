/**
 * Severity is encoded three ways at once — bar count, label, and hue — so it
 * survives colour blindness, a bad monitor, and a greyscale print. Never render
 * it by colour alone.
 *
 * Hues come from the Wong palette. `info` is neutral rather than Wong's blue
 * because blue is reserved for interaction, and an info alarm should recede.
 */
import type { Severity } from './bindings/oarfish';

export type { Severity };

/**
 * Display order, which is the reverse of the domain's escalation order: the
 * worst thing belongs at the top of a list someone is scanning at 3am.
 * Typed against the generated union, so dropping a variant in Rust breaks the
 * board's build rather than silently rendering nothing.
 */
export const SEVERITIES: readonly Severity[] = [
  'critical',
  'major',
  'minor',
  'info',
  'cleared',
] as const;

/** Filled bars out of three. Signal-strength vocabulary, for a tool that watches networks. */
export const BARS: Record<Severity, number> = {
  critical: 3,
  major: 2,
  minor: 1,
  info: 0,
  cleared: 0,
};

export const SEVERITY_TEXT: Record<Severity, string> = {
  critical: 'text-critical',
  major: 'text-major',
  minor: 'text-minor',
  info: 'text-info',
  cleared: 'text-cleared',
};

export type Density = 'compact' | 'comfortable' | 'spacious';

/** Compact is the default: this is an ops tool, and its users came for data. */
export const ROW_HEIGHT: Record<Density, string> = {
  compact: 'py-1.5',
  comfortable: 'py-2.5',
  spacious: 'py-4',
};
