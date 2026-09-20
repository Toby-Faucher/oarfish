<script lang="ts">
  /**
   * The overview island: the whole open set at a glance, read from the same
   * shared snapshot the triage list reads. No separate fetch, no second
   * source of truth. When the daemon is unreachable the strip shows dashes
   * and says so, rather than presenting zeros as a quiet night.
   */
  import { onMount } from 'svelte';
  import Severity from './Severity.svelte';
  import { SEVERITIES, type Severity as SeverityLevel } from '../lib/severity';
  import { connect, connectionAlarms, connectionStatus } from '../lib/connection.svelte';

  onMount(connect);

  let alarms = $derived(connectionAlarms());
  let status = $derived(connectionStatus());
  let unreachable = $derived(status !== 'live' && alarms.length === 0);

  let bySeverity = $derived.by(() => {
    const counts = new Map<SeverityLevel, number>(SEVERITIES.map((s) => [s, 0]));
    for (const alarm of alarms) counts.set(alarm.severity, (counts.get(alarm.severity) ?? 0) + 1);
    return SEVERITIES.map((severity) => ({ severity, count: counts.get(severity) ?? 0 }));
  });

  let hostRows = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const alarm of alarms) counts.set(alarm.host, (counts.get(alarm.host) ?? 0) + 1);
    return [...counts.entries()]
      .map(([host, count]) => ({ host, count }))
      .sort((a, b) => b.count - a.count);
  });

  let paging = $derived(alarms.filter((a) => a.lane === 'Page').length);
  let critical = $derived(alarms.filter((a) => a.severity === 'critical').length);
  let maxSeverity = $derived(Math.max(1, ...bySeverity.map((s) => s.count)));
  let maxHost = $derived(Math.max(1, ...hostRows.map((h) => h.count)));
  let cell = 'px-4 py-3';
  let num = $derived((n: number) => (unreachable ? '—' : n.toLocaleString()));
</script>

{#if unreachable}
  <div class="border-b border-line px-4 py-2 font-mono text-[11px] text-ink-3">
    Daemon unreachable. Showing nothing rather than stale numbers.
  </div>
{/if}

<div class="grid grid-cols-2 divide-x divide-line border-b border-line sm:grid-cols-4" aria-label="Totals">
  <div class={cell}>
    <div class="font-mono text-[26px] leading-none tabular-nums">{num(alarms.length)}</div>
    <div class="label mt-1.5 text-[10.5px] text-ink-3">Open alarms</div>
  </div>
  <div class={cell}>
    <div class="font-mono text-[26px] leading-none tabular-nums text-critical">{num(critical)}</div>
    <div class="label mt-1.5 text-[10.5px] text-ink-3">Critical</div>
  </div>
  <div class={cell}>
    <div class="font-mono text-[26px] leading-none tabular-nums">{num(paging)}</div>
    <div class="label mt-1.5 text-[10.5px] text-ink-3">Paging</div>
  </div>
  <div class={cell}>
    <div class="font-mono text-[26px] leading-none tabular-nums">{num(hostRows.length)}</div>
    <div class="label mt-1.5 text-[10.5px] text-ink-3">Hosts</div>
  </div>
</div>

<div class="grid items-start md:grid-cols-2">
  <div class="px-4 py-3 md:border-r md:border-line">
    <h3 class="label text-[11px] text-ink-3">By severity</h3>
    <ul class="mt-2.5 flex list-none flex-col gap-2 p-0">
      {#each bySeverity as row}
        <li class="grid grid-cols-[104px_minmax(0,1fr)_44px] items-center gap-3">
          <Severity level={row.severity} />
          <span class="h-[3px] overflow-hidden rounded-full bg-l3" aria-hidden="true">
            <span
              class="block h-full rounded-full bg-line-2"
              style={`width: ${((row.count / maxSeverity) * 100).toFixed(0)}%`}
            ></span>
          </span>
          <span class="text-right font-mono text-[12px] tabular-nums text-ink-2">{num(row.count)}</span>
        </li>
      {/each}
    </ul>
  </div>
  <div class="border-t border-line px-4 py-3 md:border-t-0">
    <h3 class="label text-[11px] text-ink-3">By host</h3>
    {#if hostRows.length === 0}
      <p class="mt-2 font-mono text-[11px] text-ink-3">No hosts firing.</p>
    {:else}
      <ul class="mt-2 flex list-none flex-col gap-1.5 p-0">
        {#each hostRows as h}
          <li class="grid grid-cols-[minmax(0,1fr)_44px] items-center gap-3 font-mono text-[11.5px]">
            <span class="flex min-w-0 items-center gap-2">
              <span class="truncate text-ink-2">{h.host}</span>
              <span class="h-[3px] min-w-0 flex-1 rounded-full bg-l3" aria-hidden="true">
                <span
                  class="block h-full rounded-full bg-line-2"
                  style={`width: ${((h.count / maxHost) * 100).toFixed(0)}%`}
                ></span>
              </span>
            </span>
            <span class="text-right tabular-nums text-ink-2">{num(h.count)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</div>
