<script lang="ts">
  import type { Density } from '../lib/severity';

  interface Props {
    value: Density;
    onchange: (d: Density) => void;
  }

  let { value, onchange }: Props = $props();

  const options: Array<{ key: Density; short: string }> = [
    { key: 'compact', short: 'Cmp' },
    { key: 'comfortable', short: 'Cmf' },
    { key: 'spacious', short: 'Spc' },
  ];
</script>

<div class="flex gap-0.5 rounded-(--radius-ui) border border-line bg-l0 p-0.5" role="group" aria-label="Row density">
  {#each options as option}
    <button
      type="button"
      onclick={() => onchange(option.key)}
      aria-pressed={value === option.key}
      title={option.key}
      class="rounded-[3px] px-2 py-1 font-cond text-[11px] font-bold tracking-[0.08em] uppercase transition-colors duration-[120ms]
             {value === option.key ? 'bg-l3 text-ink' : 'text-ink-3 hover:text-ink'}"
    >
      {option.short}
    </button>
  {/each}
</div>
