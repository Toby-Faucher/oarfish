<script lang="ts">
  import { shaderBgEnabled, setShaderBgEnabled } from '../lib/shaderBg.svelte';

  let enabled = $derived(shaderBgEnabled());
  const choices = [false, true] as const;
</script>

<div class="flex gap-0.5 rounded-(--radius-ui) border border-line bg-l0 p-0.5" role="group" aria-label="Ocean background">
  {#each choices as choice (choice)}
    <button
      type="button"
      onclick={() => setShaderBgEnabled(choice)}
      aria-pressed={enabled === choice}
      class="rounded-[3px] px-2.5 py-1 font-cond text-[11px] font-bold tracking-[0.08em] uppercase transition-colors duration-[120ms]
             {enabled === choice ? 'bg-l3 text-ink' : 'text-ink-3 hover:text-ink'}"
    >
      {choice ? 'On' : 'Off'}
    </button>
  {/each}
</div>
