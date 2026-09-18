<script lang="ts">
  /*
   * A deliberately tiny island. Its job right now is to prove hydration works:
   * if the clock is ticking, this component's JavaScript is running in the
   * browser. Everything around it on the page is static HTML.
   *
   * It becomes the real SSE connection indicator once oarfish-api exists.
   */
  import { onMount } from 'svelte';

  let now = $state(new Date());
  let connected = $state(false);

  onMount(() => {
    connected = true;
    const id = setInterval(() => (now = new Date()), 1000);
    return () => clearInterval(id);
  });

  const clock = $derived(
    now.toLocaleTimeString('en-GB', { hour12: false })
  );
</script>

<div class="flex items-center gap-2 font-mono text-xs text-ink-3">
  <span
    class="size-1.5 rounded-full"
    class:bg-ok={connected}
    class:bg-ink-3={!connected}
    aria-hidden="true"
  ></span>
  <span>{connected ? 'live' : 'connecting'}</span>
  <span class="tabular-nums">{clock}</span>
</div>
