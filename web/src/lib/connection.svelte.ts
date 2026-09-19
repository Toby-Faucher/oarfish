import type { Alarm } from './bindings/oarfish';

/**
 * The board's one live data source, shared by every island. `AlarmList` and
 * `ConnectionPulse` hydrate as separate Svelte apps, but both import this
 * module — an ES module is a singleton, so they read the same state and the
 * connection opens exactly once.
 *
 * One rule drives it, taken from `oarfish-api`'s own doc comment:
 * re-fetching `/api/alarms` is cheap and always correct. Every path that
 * could have missed something — first connect, a dropped connection's
 * reconnect, an explicit `resync` on stream lag — reduces to the same
 * re-fetch, so there is exactly one way the list becomes correct rather
 * than a special case per failure mode.
 */

export type ConnectionStatus = 'connecting' | 'live';

let alarms = $state<Alarm[]>([]);
let status = $state<ConnectionStatus>('connecting');
let started = false;

async function resync() {
  try {
    const response = await fetch('/api/alarms');
    if (response.ok) {
      alarms = await response.json();
    }
  } catch {
    // A network blip during resync leaves the list as it was; the next
    // `open` or `resync` tries again rather than clearing the board.
  }
}

function upsert(alarm: Alarm) {
  const index = alarms.findIndex((existing) => existing.id === alarm.id);
  alarms = index === -1 ? [...alarms, alarm] : alarms.with(index, alarm);
}

function remove(id: string) {
  alarms = alarms.filter((alarm) => alarm.id !== id);
}

/** Open the stream once. Safe to call from every island that mounts. */
export function connect() {
  if (started) return;
  started = true;

  const source = new EventSource('/api/alarms/stream');

  source.addEventListener('open', () => {
    status = 'live';
    void resync();
  });
  source.addEventListener('error', () => {
    // The browser retries on its own; there is no clean "gave up" signal
    // to build a third state around.
    status = 'connecting';
  });
  source.addEventListener('resync', () => void resync());
  source.addEventListener('raised', (event: MessageEvent) => {
    const change: { Raised: Alarm } = JSON.parse(event.data);
    upsert(change.Raised);
  });
  source.addEventListener('updated', (event: MessageEvent) => {
    const change: { Updated: Alarm } = JSON.parse(event.data);
    upsert(change.Updated);
  });
  source.addEventListener('cleared', (event: MessageEvent) => {
    const change: { Cleared: string } = JSON.parse(event.data);
    remove(change.Cleared);
  });
}

export function connectionAlarms(): Alarm[] {
  return alarms;
}

export function connectionStatus(): ConnectionStatus {
  return status;
}
