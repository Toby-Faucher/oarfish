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
  <!--
    The severity label is always visible: bars and hue alone drop to
    hue-only for info/cleared, which fill zero bars.
  -->
  <span class="w-[104px] shrink-0">
    <Severity level={alarm.severity} />
  </span>

  <span class="min-w-0 flex-1">
    <!--
      The display line is the masked template, in mono because it is machine
      text. Flat: the dimensioned drawing is Template.svelte's job, and only
      one component gets to be loud.
    -->
    <span class="block truncate font-mono text-[12.5px] text-ink">{alarm.template}</span>
    {#if density !== 'compact'}
      <!--
        Lane is neutral text, never a severity hue: a minor alarm routed to
        Page must not read as critical. The clock stays in the Opened column
        on wide screens and rides here only where that column is hidden.
      -->
      <span class="mt-0.5 block font-mono text-[10.5px] text-ink-3">
        {alarm.host} · <span class="text-ink-2">{alarm.lane}</span><span class="sm:hidden"> · {clockOf(alarm.opened_at)} UTC</span>
      </span>
    {:else}
      <!--
        The Page chip is the will-it-wake-me signal, so it shows at every
        width even in compact; Dashboard and Record live in the detail
        header. Host and clock ride along only where their columns hide.
      -->
      {#if alarm.lane === 'Page'}
        <span class="mt-0.5 flex items-center gap-1.5 font-mono text-[10.5px] text-ink-3">
          <span class="rounded-(--radius-ui) bg-l3 px-1.5 py-px text-[10px] text-ink-2">Page</span>
          <span class="sm:hidden">{alarm.host} · {clockOf(alarm.opened_at)} UTC</span>
        </span>
      {:else}
        <span class="mt-0.5 block font-mono text-[10.5px] text-ink-3 sm:hidden">
          {alarm.host} · {clockOf(alarm.opened_at)} UTC
        </span>
      {/if}
    {/if}
  </span>

  <span class="hidden truncate text-right font-mono text-[11px] text-ink-3 sm:block">{alarm.host}</span>

  <!-- Counts are compared down a column, so: right-aligned and tabular. -->
  <span class="hidden text-right font-mono text-[14px] tabular-nums text-ink sm:block">{alarm.count}</span>

  <span class="flex items-center justify-end gap-1 font-mono text-[10.5px] tabular-nums text-ink-3">
    {#if density === 'compact'}
      <span class="sm:hidden">{alarm.count}</span>
    {/if}
    <!-- UTC suffixed, and hidden where the meta line already carries it. -->
    <span class="hidden sm:inline">{clockOf(alarm.opened_at)} UTC</span>
    <ChevronRight
      size={14}
      class="shrink-0 transition-colors duration-[120ms] {selected ? 'text-accent' : 'text-ink-3'}"
    />
  </span>
</button>
