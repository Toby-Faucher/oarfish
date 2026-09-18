<script lang="ts">
  import { BARS, SEVERITY_TEXT, type Severity } from '../lib/severity';

  interface Props {
    level: Severity;
    /** Hide the text label in space-constrained rows. Bars and hue still carry it. */
    compact?: boolean;
  }

  let { level, compact = false }: Props = $props();

  const filled = $derived(BARS[level]);
  const tone = $derived(SEVERITY_TEXT[level]);
</script>

<div class="flex shrink-0 items-center gap-2 {tone}" title={level}>
  <span class="flex h-[11px] items-end gap-[2px]" aria-hidden="true">
    {#each [5, 8, 11] as h, i}
      <span
        class="block w-[3px] rounded-[0.5px] bg-current transition-opacity duration-150"
        style="height: {h}px; opacity: {i < filled ? 1 : 0.22}"
      ></span>
    {/each}
  </span>
  {#if !compact}
    <span class="label text-[12px]">{level}</span>
  {:else}
    <span class="sr-only">{level}</span>
  {/if}
</div>
