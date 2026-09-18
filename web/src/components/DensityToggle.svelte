<script lang="ts">
  import type { Density } from '../lib/severity';

  interface Props {
    value: Density;
    onchange: (d: Density) => void;
  }

  let { value, onchange }: Props = $props();

  const options: Density[] = ['compact', 'comfortable', 'spacious'];
</script>

<div class="flex gap-0.5 rounded-(--radius-ui) bg-l2 p-0.5" role="group" aria-label="Row density">
  {#each options as option}
    <button
      type="button"
      onclick={() => onchange(option)}
      aria-pressed={value === option}
      class="rounded-[3px] px-2 py-1 font-mono text-[10.5px] transition-colors duration-[120ms]
             {value === option ? 'bg-accent text-accent-fg' : 'text-ink-3 hover:text-ink'}"
    >
      {option}
    </button>
  {/each}
</div>
