<script lang="ts">
  import { THEMES, currentTheme, setTheme } from '../lib/theme.svelte';
</script>

<div class="flex gap-0.5 rounded-(--radius-ui) border border-line bg-l0 p-0.5" role="group" aria-label="Colour theme">
  {#each THEMES as theme}
    <button
      type="button"
      onclick={() => setTheme(theme.id)}
      aria-pressed={currentTheme() === theme.id}
      class="rounded-[3px] px-2.5 py-1 font-cond text-[11px] font-bold tracking-[0.08em] uppercase transition-colors duration-[120ms]
             {currentTheme() === theme.id ? 'bg-l3 text-ink' : 'text-ink-3 hover:text-ink'}"
    >
      {theme.label}
    </button>
  {/each}
</div>
