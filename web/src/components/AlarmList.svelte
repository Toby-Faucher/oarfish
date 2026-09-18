<script lang="ts">
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import AlarmRow from './AlarmRow.svelte';
  import type { Alarm } from '../lib/bindings/oarfish';
  import DensityToggle from './DensityToggle.svelte';
  import type { Density } from '../lib/severity';

  interface Props {
    alarms: Alarm[];
  }

  let { alarms }: Props = $props();

  let density = $state<Density>('compact');

  /*
   * Motion budget, moment one of two: a new alarm enters the list. It is rare,
   * it is the only thing you came for, and it should be noticeable from across
   * a room. Nothing else in this component animates beyond a colour change.
   */
  const ENTER = { y: -8, duration: 220, easing: cubicOut };
</script>

<div class="flex items-center gap-3 border-b border-line bg-l1 px-3.5 py-2">
  <h2 class="label text-[13px]">Open alarms</h2>
  <span class="rounded-(--radius-ui) bg-l2 px-1.5 py-0.5 font-mono text-[11px] tabular-nums text-ink-2">
    {alarms.length}
  </span>
  <div class="ml-auto">
    <DensityToggle value={density} onchange={(d) => (density = d)} />
  </div>
</div>

{#if alarms.length === 0}
  <!-- An empty screen is an invitation, not a shrug. -->
  <div class="px-3.5 py-10 text-center">
    <p class="text-[13.5px] text-ink-2">Nothing is firing.</p>
    <p class="mt-1 font-mono text-[11px] text-ink-3">
      Oarfish is watching. It will wake you only if something earns it.
    </p>
  </div>
{:else}
  <ul class="list-none p-0">
    {#each alarms as alarm (alarm.id)}
      <li in:fly={ENTER}>
        <AlarmRow {alarm} {density} />
      </li>
    {/each}
  </ul>
{/if}
