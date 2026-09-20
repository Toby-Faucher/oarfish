<script lang="ts">
  /**
   * The triage island: the board's first job is working through open alarms,
   * so filtering, the list, and the detail live in one component sharing one
   * selection. The list reads the live SSE snapshot; the forensic sections
   * (template, verdicts, hosts, raw lines) are example content until the
   * daemon serves live detail, and the panel says so with an Example badge.
   */
  import { onMount } from 'svelte';
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import AlarmRow from './AlarmRow.svelte';
  import DensityToggle from './DensityToggle.svelte';
  import Severity from './Severity.svelte';
  import Template from './Template.svelte';
  import type { Density, Severity as SeverityLevel } from '../lib/severity';
  import type { Alarm, Slot } from '../lib/bindings/oarfish';
  import { connect, connectionAlarms } from '../lib/connection.svelte';

  interface Verdict {
    label: string;
    p: number;
  }

  interface HostCount {
    name: string;
    count: number;
  }

  interface Props {
    /** Shown in the detail panel when no live alarm is selected. */
    fallback: Alarm;
    template: string;
    slots: Slot[];
    raw: string[];
    verdicts: Verdict[];
    hosts: HostCount[];
    buckets: number[];
  }

  let { fallback, template, slots, raw, verdicts, hosts, buckets }: Props = $props();

  onMount(connect);

  let live = $derived(connectionAlarms());
  let density = $state<Density>('compact');
  let query = $state('');
  let severityFilter = $state<SeverityLevel | 'all'>('all');
  let laneFilter = $state<'all' | 'Page' | 'Dashboard' | 'Record'>('all');
  let sortBy = $state<'opened' | 'count'>('opened');
  let selectedId = $state<string | null>(null);

  /*
   * Motion budget, moment one of two: a new alarm enters the list. It is rare,
   * it is the only thing you came for, and it should be noticeable from across
   * a room. Nothing else here animates beyond a colour change.
   */
  const ENTER = { y: -8, duration: 220, easing: cubicOut };

  const SEV_FILTERS: Array<SeverityLevel | 'all'> = ['all', 'critical', 'major', 'minor', 'info'];

  let filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    let rows = live.filter((a) => {
      if (severityFilter !== 'all' && a.severity !== severityFilter) return false;
      if (laneFilter !== 'all' && a.lane !== laneFilter) return false;
      if (q && !`${a.template} ${a.host}`.toLowerCase().includes(q)) return false;
      return true;
    });
    return [...rows].sort((a, b) =>
      sortBy === 'count' ? b.count - a.count : b.opened_at.localeCompare(a.opened_at),
    );
  });

  let selected = $derived(
    (selectedId !== null && live.find((a) => a.id === selectedId)) || fallback,
  );
  let showingExample = $derived(selected.id === fallback.id);

  let isFiltered = $derived(
    query.trim() !== '' || severityFilter !== 'all' || laneFilter !== 'all',
  );

  function clearFilters() {
    query = '';
    severityFilter = 'all';
    laneFilter = 'all';
  }

  /* Static example series for the detail sparkline. */
  const W = 260;
  const H = 44;
  const maxBucket = $derived(Math.max(...buckets));
  const pts = $derived(
    buckets
      .map(
        (v, i) =>
          `${((i / (buckets.length - 1)) * W).toFixed(1)},${(H - 3 - (v / maxBucket) * (H - 8)).toFixed(1)}`,
      )
      .join(' '),
  );
  const lastX = W.toFixed(1);
  const lastY = $derived((H - 3 - (buckets[buckets.length - 1] / maxBucket) * (H - 8)).toFixed(1));
  const hostMax = $derived(Math.max(...hosts.map((h) => h.count)));
  const topVerdict = $derived(verdicts[0].label);
</script>

<section class="overflow-hidden rounded-(--radius-ui) border border-line bg-l1" aria-label="Alarm triage">
  <div class="flex items-center gap-3 border-b border-line px-3.5 py-2">
    <h2 class="label text-[13px]">Open alarms</h2>
    <span class="rounded-(--radius-ui) bg-l2 px-1.5 py-0.5 font-mono text-[11px] tabular-nums text-ink-2">
      {live.length}
    </span>
    {#if isFiltered}
      <span class="font-mono text-[10.5px] tracking-[0.06em] text-ink-3 uppercase">
        filtered · {filtered.length} of {live.length}
      </span>
      <button
        type="button"
        onclick={clearFilters}
        class="rounded-(--radius-ui) px-1.5 py-0.5 font-mono text-[10.5px] text-accent transition-colors duration-[120ms] hover:bg-accent-soft"
      >
        Clear
      </button>
    {/if}
    <div class="ml-auto">
      <DensityToggle value={density} onchange={(d) => (density = d)} />
    </div>
  </div>

  <div class="flex flex-wrap items-center gap-2 border-b border-line px-3.5 py-2">
    <label class="min-w-0 flex-1 basis-44">
      <span class="sr-only">Filter by template or host</span>
      <input
        type="search"
        bind:value={query}
        placeholder="Filter template or host"
        autocomplete="off"
        spellcheck="false"
        class="w-full rounded-(--radius-ui) border border-line bg-l0 px-2 py-1.5 font-mono text-[12px] text-ink placeholder:text-ink-3 focus:border-accent focus:outline-none"
      />
    </label>
    <label class="flex items-center gap-1.5 font-mono text-[11px] text-ink-3">
      <span class="sr-only">Lane</span>
      <select
        bind:value={laneFilter}
        class="rounded-(--radius-ui) border border-line bg-l0 px-1.5 py-1.5 text-ink-2 focus:border-accent focus:outline-none"
      >
        <option value="all">All lanes</option>
        <option value="Page">Page</option>
        <option value="Dashboard">Dashboard</option>
        <option value="Record">Record</option>
      </select>
    </label>
    <label class="flex items-center gap-1.5 font-mono text-[11px] text-ink-3">
      <span class="sr-only">Sort</span>
      <select
        bind:value={sortBy}
        class="rounded-(--radius-ui) border border-line bg-l0 px-1.5 py-1.5 text-ink-2 focus:border-accent focus:outline-none"
      >
        <option value="opened">Newest</option>
        <option value="count">Count</option>
      </select>
    </label>
  </div>

  <div class="flex flex-wrap items-center gap-1 border-b border-line px-3.5 py-1.5" role="group" aria-label="Severity filter">
    {#each SEV_FILTERS as f}
      {@const active = severityFilter === f}
      <button
        type="button"
        onclick={() => (severityFilter = f)}
        aria-pressed={active}
        class="flex items-center gap-1.5 rounded-(--radius-ui) px-2 py-1 font-cond text-[11.5px] font-bold tracking-[0.08em] uppercase transition-colors duration-[120ms] {active
          ? 'bg-accent-soft text-accent'
          : 'text-ink-3 hover:bg-l2 hover:text-ink'}"
      >
        {#if f !== 'all'}
          <Severity level={f} compact />
        {/if}
        {f === 'all' ? 'All' : f}
      </button>
    {/each}
  </div>

  <div class="grid items-start lg:grid-cols-[minmax(0,1.35fr)_minmax(0,1fr)]">
    <div aria-label="Alarm list">
      {#if live.length === 0}
        <div class="px-3.5 py-10 text-center">
          <p class="text-[13.5px] text-ink-2">Nothing is firing.</p>
          <p class="mt-1 font-mono text-[11px] text-ink-3">
            Oarfish is watching. It will wake you only if something earns it.
          </p>
        </div>
      {:else if filtered.length === 0}
        <div class="px-3.5 py-10 text-center">
          <p class="text-[13.5px] text-ink-2">No alarms match these filters.</p>
          <button
            type="button"
            onclick={clearFilters}
            class="mt-2 rounded-(--radius-ui) px-2 py-1 font-mono text-[11.5px] text-accent transition-colors duration-[120ms] hover:bg-accent-soft"
          >
            Clear filters
          </button>
        </div>
      {:else}
        <div
          class="grid grid-cols-[104px_minmax(0,1fr)_64px] items-center gap-3 border-b border-line px-3.5 py-1.5 sm:grid-cols-[104px_minmax(0,1fr)_96px_72px_52px]"
          aria-hidden="true"
        >
          <span class="label text-[10px] text-ink-3">Severity</span>
          <span class="label text-[10px] text-ink-3">Template</span>
          <span class="label hidden text-right text-[10px] text-ink-3 sm:block">Host</span>
          <span class="label hidden text-right text-[10px] text-ink-3 sm:block">Count{sortBy === 'count' ? ' ↓' : ''}</span>
          <span class="label text-right text-[10px] text-ink-3">Age</span>
        </div>
        <ul class="list-none p-0">
          {#each filtered as alarm (alarm.id)}
            <li in:fly={ENTER}>
              <AlarmRow
                {alarm}
                {density}
                selected={selectedId === alarm.id}
                onselect={() => (selectedId = selectedId === alarm.id ? null : alarm.id)}
              />
            </li>
          {/each}
        </ul>
        <div class="flex items-center gap-2 border-t border-line px-3.5 py-2 font-mono text-[10.5px] tabular-nums text-ink-3">
          <span>{filtered.length} shown</span>
          <span aria-hidden="true">·</span>
          <span>{live.length} open</span>
        </div>
      {/if}
    </div>

    <aside class="border-t border-line lg:border-t-0 lg:border-l" aria-label="Alarm detail">
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-line bg-l2 px-3.5 py-2">
        <Severity level={selected.severity} />
        <span class="font-mono text-[11px] text-ink-3">{selected.template_id}</span>
        <span class="rounded-(--radius-ui) bg-l3 px-1.5 py-0.5 font-mono text-[10px] text-ink-2">{selected.lane}</span>
        {#if showingExample}
          <span class="rounded-(--radius-ui) border border-line-2 px-1.5 py-0.5 font-mono text-[10px] tracking-[0.06em] text-ink-3 uppercase">Example</span>
        {/if}
        <span class="ml-auto font-mono text-[11px] tabular-nums text-ink-2">
          {selected.count} hits · {selected.host}
        </span>
      </div>

      <div class="border-b border-line px-4 pt-3 pb-4">
        <h3 class="label text-[11px] text-ink-3">Hits per 5 min</h3>
        <svg
          viewBox={`0 0 ${W} ${H}`}
          class="mt-2 block w-full"
          role="img"
          aria-label="Hits rising over the last 50 minutes"
        >
          <polyline
            points={pts}
            fill="none"
            stroke="var(--ui-ink-3)"
            stroke-width="1.5"
            stroke-linejoin="round"
            stroke-linecap="round"
          />
          <circle cx={lastX} cy={lastY} r="3" fill="var(--ui-major)" />
        </svg>
        <div class="mt-1 flex justify-between font-mono text-[10px] tabular-nums text-ink-3" aria-hidden="true">
          <span>−50 min</span>
          <span>now</span>
        </div>
      </div>

      <div class="border-b border-line p-4">
        <Template {template} {slots} />
      </div>

      <div class="border-b border-line px-4 py-3">
        <h3 class="label text-[11px] text-ink-3">Why it fired</h3>
        <ul class="mt-2.5 flex list-none flex-col gap-2 p-0">
          {#each verdicts as v}
            <li class="grid grid-cols-[minmax(0,1fr)_44px] items-center gap-3">
              <span class="min-w-0">
                <span class="block truncate text-[12.5px] {v.label === topVerdict ? 'text-ink' : 'text-ink-2'}">{v.label}</span>
                <span
                  class="mt-1 block h-[3px] overflow-hidden rounded-full bg-l3"
                  role="img"
                  aria-label={`${v.label}: ${(v.p * 100).toFixed(0)} percent`}
                >
                  <span
                    class="block h-full rounded-full {v.label === topVerdict ? 'bg-major' : 'bg-line-2'}"
                    style={`width: ${(v.p * 100).toFixed(0)}%`}
                  ></span>
                </span>
              </span>
              <span class="text-right font-mono text-[12px] tabular-nums text-ink-2">{v.p.toFixed(2)}</span>
            </li>
          {/each}
        </ul>
      </div>

      <div class="border-b border-line px-4 py-3">
        <h3 class="label text-[11px] text-ink-3">Hosts</h3>
        <ul class="mt-2 flex list-none flex-col gap-1.5 p-0">
          {#each hosts as h}
            <li class="grid grid-cols-[minmax(0,1fr)_44px] items-center gap-3 font-mono text-[11.5px]">
              <span class="flex min-w-0 items-center gap-2">
                <span class="truncate text-ink-2">{h.name}</span>
                <span class="h-[3px] min-w-0 flex-1 rounded-full bg-l3" aria-hidden="true">
                  <span
                    class="block h-full rounded-full bg-line-2"
                    style={`width: ${((h.count / hostMax) * 100).toFixed(0)}%`}
                  ></span>
                </span>
              </span>
              <span class="text-right tabular-nums text-ink-2">{h.count}</span>
            </li>
          {/each}
        </ul>
        <dl class="mt-3 grid grid-cols-[96px_minmax(0,1fr)] gap-x-3 gap-y-1 font-mono text-[11.5px]">
          <dt class="text-ink-3">First seen</dt>
          <dd class="m-0 tabular-nums text-ink-2">03:12 UTC</dd>
          <dt class="text-ink-3">Last seen</dt>
          <dd class="m-0 tabular-nums text-ink-2">04:02 UTC</dd>
        </dl>
      </div>

      <div class="border-b border-line px-4 py-3">
        <h3 class="label text-[11px] text-ink-3">Raw lines</h3>
        <pre class="mt-2 overflow-x-auto rounded-(--radius-ui) border border-line bg-l0 p-3 font-mono text-[11.5px] leading-relaxed text-ink-2">{raw.join('\n')}</pre>
        <p class="mt-2 font-mono text-[10.5px] text-ink-3">Kept verbatim. What arrived is what you read.</p>
      </div>

      <div class="border-b border-line px-4 py-3">
        <h3 class="label text-[11px] text-ink-3">Decision record</h3>
        <dl class="mt-2 grid grid-cols-[96px_minmax(0,1fr)] gap-x-3 gap-y-1 font-mono text-[11.5px]">
          <dt class="text-ink-3">Model</dt>
          <dd class="m-0 text-ink-2">typesafe/jev-1.13</dd>
          <dt class="text-ink-3">Resolved</dt>
          <dd class="m-0 text-ink-2">typesafe/jev-1.13-20260917</dd>
          <dt class="text-ink-3">Confidence</dt>
          <dd class="m-0 tabular-nums text-ink-2">0.81</dd>
          <dt class="text-ink-3">Actionable</dt>
          <dd class="m-0 text-ink-2">yes</dd>
        </dl>
      </div>

      <div class="flex flex-wrap gap-2 px-4 py-3">
        <button
          type="button"
          class="rounded-(--radius-ui) bg-accent px-3 py-1.5 text-[13px] font-medium text-accent-fg transition-colors duration-[120ms] hover:brightness-110"
        >
          Acknowledge
        </button>
        <button
          type="button"
          class="rounded-(--radius-ui) border border-line px-3 py-1.5 text-[13px] text-ink-2 transition-colors duration-[120ms] hover:bg-l2 hover:text-ink"
        >
          Not an alarm
        </button>
      </div>
    </aside>
  </div>
</section>
