<script lang="ts">
  /**
   * Dev-only settings control for the demo alarm — replaces the old `?demo`
   * URL param with a persistent toggle. Renders only when Astro's own
   * `import.meta.env.DEV` check in the page keeps this island out of a
   * production build entirely; this component's own DEV guard is a second,
   * redundant safety net in case it's ever mounted somewhere that check
   * didn't cover.
   */
  import { demoModeEnabled, setDemoMode } from '../lib/settings.svelte';

  let enabled = $derived(demoModeEnabled());
  const choices = [false, true] as const;
</script>

{#if import.meta.env.DEV}
  <div class="flex gap-0.5 rounded-(--radius-ui) border border-line bg-l0 p-0.5" role="group" aria-label="Demo alarm">
    {#each choices as choice (choice)}
      <button
        type="button"
        onclick={() => setDemoMode(choice)}
        aria-pressed={enabled === choice}
        class="rounded-[3px] px-2 py-1 font-cond text-[11px] font-bold tracking-[0.08em] uppercase transition-colors duration-[120ms]
               {enabled === choice ? 'bg-l3 text-ink' : 'text-ink-3 hover:text-ink'}"
      >
        {choice ? 'On' : 'Off'}
      </button>
    {/each}
  </div>
{/if}
