<script lang="ts">
  /**
   * The triage island: the board's first job is working through open alarms,
   * so filtering, the list, and the detail live in one component sharing one
   * selection. The list reads the live SSE snapshot; selecting an alarm
   * fetches its forensics from `/api/alarms/{id}/detail`. Nothing here is
   * mock: no selection shows a prompt, a failed fetch shows an error with a
   * retry, and an unjudged alarm says so.
   */
  import { onMount } from 'svelte';
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import AlarmRow from './AlarmRow.svelte';
  import DensityToggle from './DensityToggle.svelte';
  import Severity from './Severity.svelte';
  import Template from './Template.svelte';
  import type { Density, Severity as SeverityLevel } from '../lib/severity';
  import type { AlarmDetail, VerdictAnswer } from '../lib/bindings/oarfish';
  import { connect, connectionAlarms } from '../lib/connection.svelte';
  import { clockOf } from '../lib/time';

  onMount(connect);

  let live = $derived(connectionAlarms());
  let density = $state<Density>('compact');
  let query = $state('');
  let severityFilter = $state<SeverityLevel | 'all'>('all');
  let laneFilter = $state<'all' | 'Page' | 'Dashboard' | 'Record'>('all');
  let sortBy = $state<'opened' | 'count'>('opened');
  let selectedId = $state<string | null>(null);
  let retryNonce = $state(0);

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

  /* A cleared alarm leaves no dangling selection behind. */
  let selected = $derived(selectedId !== null ? (live.find((a) => a.id === selectedId) ?? null) : null);

  let isFiltered = $derived(
    query.trim() !== '' || severityFilter !== 'all' || laneFilter !== 'all',
  );

  function clearFilters() {
    query = '';
    severityFilter = 'all';
    laneFilter = 'all';
  }

  /*
   * Forensics for the selected alarm. A late arrival for a previous row is
   * discarded rather than painted under the wrong alarm.
   */
  let detail = $state<AlarmDetail | null>(null);
  let detailState = $state<'idle' | 'loading' | 'live' | 'error'>('idle');

  $effect(() => {
    retryNonce;
    if (selected === null) {
      detail = null;
      detailState = 'idle';
      return;
    }
    const id = selected.id;
    detailState = 'loading';
    let cancelled = false;
    fetch(`/api/alarms/${id}/detail`)
      .then((response) =>
        response.ok ? response.json() : Promise.reject(new Error(String(response.status))),
      )
      .then((loaded: AlarmDetail) => {
        if (!cancelled) {
          detail = loaded;
          detailState = 'live';
        }
      })
      .catch(() => {
        if (!cancelled) {
          detail = null;
          detailState = 'error';
        }
      });
    return () => {
      cancelled = true;
    };
  });

  let liveDetail = $derived(
    detailState === 'live' && detail !== null && selected !== null && detail.alarm.id === selected.id
      ? detail
      : null,
  );

  function distributionOf(answer: VerdictAnswer): Array<{ label: string; p: number }> {
    if ('choice' in answer) return entriesOf(answer.choice.probabilities);
    if ('score' in answer) return entriesOf(answer.score.probabilities);
    return [{ label: 'yes', p: answer.noul.noul }];
  }

  function entriesOf(probabilities: { [key in string]: number }): Array<{
    label: string;
    p: number;
  }> {
    return Object.entries(probabilities)
      .map(([label, p]) => ({ label, p }))
      .sort((a, b) => b.p - a.p)
      .slice(0, 4);
  }

  function headlineOf(answer: VerdictAnswer): { value: string; confidence: number | null } {
    if ('choice' in answer) return { value: answer.choice.choice, confidence: answer.choice.confidence };
    if ('score' in answer)
      return { value: answer.score.score.toFixed(1), confidence: answer.score.confidence };
    // `noul` carries no separate confidence on the wire; its value is the
    // probability, so none is shown rather than one invented.
    return { value: answer.noul.noul >= 0.5 ? 'yes' : 'no', confidence: null };
  }
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
      {#if selected === null}
        <div class="px-4 py-10 text-center">
          <p class="text-[13.5px] text-ink-2">No alarm selected.</p>
          <p class="mt-1 font-mono text-[11px] text-ink-3">Select a row to inspect it.</p>
        </div>
      {:else}
        <div class="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-line bg-l2 px-3.5 py-2">
          <Severity level={selected.severity} />
          <span class="font-mono text-[11px] text-ink-3">{selected.template_id}</span>
          <span class="rounded-(--radius-ui) bg-l3 px-1.5 py-0.5 font-mono text-[10px] text-ink-2">{selected.lane}</span>
          {#if liveDetail}
            <span class="rounded-(--radius-ui) bg-accent-soft px-1.5 py-0.5 font-mono text-[10px] tracking-[0.06em] text-accent uppercase">Live</span>
          {/if}
          <span class="ml-auto font-mono text-[11px] tabular-nums text-ink-2">
            {selected.count} hits · {selected.host}
          </span>
        </div>

        {#if liveDetail}
          {@const forensic = liveDetail}
          <div class="border-b border-line p-4">
            <Template template={forensic.alarm.template} slots={[]} />
          </div>

          <div class="border-b border-line px-4 py-3">
            <h3 class="label text-[11px] text-ink-3">Why it fired</h3>
            {#if forensic.verdict}
              {@const answers = Object.entries(forensic.verdict.answers)}
              <div class="mt-2.5 flex flex-col gap-4">
                {#each answers as [qid, answer]}
                  {@const head = headlineOf(answer)}
                  {@const rows = distributionOf(answer)}
                  {@const top = rows.length ? rows[0].label : ''}
                  <div>
                    <div class="flex items-baseline gap-2">
                      <h4 class="label text-[10.5px] text-ink-2">{qid}</h4>
                      <span class="truncate text-[12.5px] text-ink">{head.value}</span>
                      {#if head.confidence !== null}
                        <span class="ml-auto shrink-0 font-mono text-[11px] tabular-nums text-ink-3">
                          conf {head.confidence.toFixed(2)}
                        </span>
                      {/if}
                    </div>
                    <ul class="mt-1.5 flex list-none flex-col gap-1.5 p-0">
                      {#each rows as row}
                        <li class="grid grid-cols-[minmax(0,1fr)_44px] items-center gap-3">
                          <span class="min-w-0">
                            <span class="block truncate text-[12px] {row.label === top ? 'text-ink' : 'text-ink-2'}">{row.label}</span>
                            <span
                              class="mt-1 block h-[3px] overflow-hidden rounded-full bg-l3"
                              role="img"
                              aria-label={`${row.label}: ${(row.p * 100).toFixed(0)} percent`}
                            >
                              <span
                                class="block h-full rounded-full {row.label === top ? 'bg-major' : 'bg-line-2'}"
                                style={`width: ${(row.p * 100).toFixed(0)}%`}
                              ></span>
                            </span>
                          </span>
                          <span class="text-right font-mono text-[11.5px] tabular-nums text-ink-2">{row.p.toFixed(2)}</span>
                        </li>
                      {/each}
                    </ul>
                  </div>
                {/each}
              </div>
            {:else}
              <p class="mt-2 text-[12.5px] text-ink-2">Not yet judged. The verdict lands here when Jev answers.</p>
            {/if}
          </div>

          <div class="border-b border-line px-4 py-3">
            <h3 class="label text-[11px] text-ink-3">Decision record</h3>
            {#if forensic.record}
              <dl class="mt-2 grid grid-cols-[96px_minmax(0,1fr)] gap-x-3 gap-y-1 font-mono text-[11.5px]">
                <dt class="text-ink-3">Model</dt>
                <dd class="m-0 break-all text-ink-2">{forensic.record.model}</dd>
                <dt class="text-ink-3">Recorded</dt>
                <dd class="m-0 tabular-nums text-ink-2">{clockOf(forensic.record.recorded_at)} UTC</dd>
                <dt class="text-ink-3">Tokens</dt>
                <dd class="m-0 tabular-nums text-ink-2">
                  {forensic.record.input_tokens.toLocaleString()} in · {forensic.record.output_tokens.toLocaleString()} out
                </dd>
                {#if forensic.record.cost !== null && forensic.record.cost !== undefined}
                  <dt class="text-ink-3">Cost</dt>
                  <dd class="m-0 tabular-nums text-ink-2">${forensic.record.cost.toFixed(4)}</dd>
                {/if}
              </dl>
            {:else}
              <p class="mt-2 text-[12.5px] text-ink-2">No decision record yet.</p>
            {/if}
          </div>
        {:else if detailState === 'loading'}
          <div class="px-4 py-10 text-center">
            <p class="font-mono text-[11.5px] text-ink-3">Reading forensics…</p>
          </div>
        {:else}
          <div class="px-4 py-10 text-center">
            <p class="text-[13.5px] text-ink-2">Forensics unavailable.</p>
            <p class="mt-1 font-mono text-[11px] text-ink-3">The daemon did not answer.</p>
            <button
              type="button"
              onclick={() => (retryNonce += 1)}
              class="mt-2 rounded-(--radius-ui) px-2 py-1 font-mono text-[11.5px] text-accent transition-colors duration-[120ms] hover:bg-accent-soft"
            >
              Retry
            </button>
          </div>
        {/if}
      {/if}

      {#if selected !== null}
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
      {/if}
    </aside>
  </div>
</section>
