<script lang="ts">
  import Severity from './Severity.svelte';
  import { ROW_HEIGHT, type Density } from '../lib/severity';
  import type { Alarm } from '../lib/bindings/oarfish';
  import { clockOf } from '../lib/time';

  interface Props {
    alarm: Alarm;
    density?: Density;
    selected?: boolean;
  }

  let { alarm, density = 'compact', selected = false }: Props = $props();
</script>

<!--
  No left-edge accent bar. Severity is a real column carrying bars, label and
  hue, which is both more informative and not the AI-interface tell.
-->
<div
  class="flex items-center gap-3 border-b border-line px-3.5 transition-colors duration-[120ms] last:border-b-0 hover:bg-l2 {ROW_HEIGHT[
    density
  ]} {selected ? 'bg-l2' : ''}"
>
  <div class="w-[104px] shrink-0">
    <Severity level={alarm.severity} compact={density === 'compact'} />
  </div>

  <div class="min-w-0 flex-1">
    <!--
      The display line is the masked template, in mono because it is machine
      text. Flat: the dimensioned drawing is Template.svelte's job, and only
      one component gets to be loud.
    -->
    <div class="truncate font-mono text-[12.5px]">{alarm.template}</div>
    {#if density !== 'compact'}
      <div class="mt-0.5 font-mono text-[10.5px] text-ink-3">{alarm.host} · {clockOf(alarm.opened_at)}</div>
    {/if}
  </div>

  {#if density === 'compact'}
    <div class="shrink-0 font-mono text-[10.5px] text-ink-3">{alarm.host}</div>
  {/if}

  <!-- Counts are compared down a column, so: right-aligned and tabular. -->
  <div class="w-14 shrink-0 text-right font-mono text-[14px] tabular-nums">{alarm.count}</div>
</div>
