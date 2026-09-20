/**
 * `opened_at` crosses the wire as RFC 3339. The board shows a bare clock time,
 * because the date is almost always today and the column is scanned, not read.
 *
 * Fixed to UTC on purpose. The list is a server-rendered island, so formatting
 * in the server's zone and then rehydrating in the viewer's would flash a
 * different time on load. Showing local time correctly needs the island to
 * re-format after hydration, which lands with the SSE wiring in M5.
 */
const CLOCK = new Intl.DateTimeFormat('en-GB', {
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

export function clockOf(iso: string): string {
  return CLOCK.format(new Date(iso));
}

/*
 * The full stamp, with date: the detail header and the decision record name
 * multi-day alarms, where a bare clock is ambiguous. UTC for the same reason
 * as above, and always suffixed at the call site.
 */
const STAMP = new Intl.DateTimeFormat('en-GB', {
  day: '2-digit',
  month: 'short',
  year: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

export function stampOf(iso: string): string {
  return STAMP.format(new Date(iso));
}
