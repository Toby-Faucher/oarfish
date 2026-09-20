<script lang="ts">
  import Severity from './Severity.svelte';
  import { ChevronRight } from '@lucide/svelte';
  import { ROW_HEIGHT, type Density } from '../lib/severity';
  import type { Alarm } from '../lib/bindings/oarfish';
  import { clockOf } from '../lib/time';

  interface Props {
    alarm: Alarm;
    density?: Density;
    selected?: boolean;
    onselect?: () => void;
  }

  let { alarm, density = 'compact', selected = false, onselect }: Props = $props();

  const LANE_TONE: Record<Alarm['lane'], string> = {
    Page: 'text-critical',
    Dashboard: 'text-ink-2',
    Record: 'text-ink-3',
  };
</script>

<!--
  No left-edge accent bar. Severity is a real column carrying bars, label and
  hue, which is both more informative and not the AI-interface tell.
  The inspect control is always visible: no hover-only actions.
-->
<button
  type="button"
  onclick={onselect}
  aria-expanded={selected}
  class="grid w-full grid-cols-[104px_minmax(0,1fr)_64px] items-center gap-3 border-b border-line px-3.5 text-left transition-colors duration-[120ms] last:border-b-0 hover:bg-l2 sm:grid-cols-[104px_minmax(0,1fr)_96px_72px_52px] {ROW_HEIGHT[
    density
  ]} {selected ? 'bg-l2' : ''}"
>
  <span class="w-[104px] shrink-0">
    <Severity level={alarm.severity} compact={density === 'compact'} />
  </span>

  <span class="min-w-0 flex-1">
    <!--
      The display line is the masked template, in mono because it is machine
      text. Flat: the dimensioned drawing is Template.svelte's job, and only
      one component gets to be loud.
    -->
    <span class="block truncate font-mono text-[12.5px] text-ink">{alarm.template}</span>
    {#if density !== 'compact'}
      <span class="mt-0.5 block font-mono text-[10.5px] text-ink-3">
        {alarm.host} · {clockOf(alarm.opened_at)} UTC · <span class={LANE_TONE[alarm.lane]}>{alarm.lane}</span>
      </span>
    {/if}
    {#if density === 'compact'}
      <span class="mt-0.5 block font-mono text-[10.5px] text-ink-3 sm:hidden">
        {alarm.host} · {clockOf(alarm.opened_at)}
      </span>
    {/if}
  </span>

  <span class="hidden truncate text-right font-mono text-[11px] text-ink-3 sm:block">{alarm.host}</span>

  <!-- Counts are compared down a column, so: right-aligned and tabular. -->
  <span class="hidden text-right font-mono text-[14px] tabular-nums text-ink sm:block">{alarm.count}</span>

  <span class="flex items-center justify-end gap-1 font-mono text-[10.5px] tabular-nums text-ink-3">
    {#if density === 'compact'}
      <span class="sm:hidden">{alarm.count}</span>
    {/if}
    {clockOf(alarm.opened_at)}
    <ChevronRight
      size={14}
      class="shrink-0 transition-colors duration-[120ms] {selected ? 'text-accent' : 'text-ink-3'}"
    />
  </span>
</button>
