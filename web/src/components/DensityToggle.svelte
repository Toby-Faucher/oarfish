<script lang="ts">
  import type { Density } from '../lib/severity';

  interface Props {
    value: Density;
    onchange: (d: Density) => void;
  }

  let { value, onchange }: Props = $props();

  // Full words, always visible: abbreviations behind a hover tooltip are
  // absent on touch and unreadable at a glance.
  const options: Array<{ key: Density; label: string }> = [
    { key: 'compact', label: 'Compact' },
    { key: 'comfortable', label: 'Comfort' },
    { key: 'spacious', label: 'Spacious' },
  ];
</script>

<div class="flex gap-0.5 rounded-(--radius-ui) border border-line bg-l0 p-0.5" role="group" aria-label="Row density">
  {#each options as option}
    <button
      type="button"
      onclick={() => onchange(option.key)}
      aria-pressed={value === option.key}
      class="rounded-[3px] px-2 py-1 font-cond text-[11px] font-bold tracking-[0.08em] uppercase transition-colors duration-[120ms]
             {value === option.key ? 'bg-l3 text-ink' : 'text-ink-3 hover:text-ink'}"
    >
      {option.label}
    </button>
  {/each}
</div>
