<script lang="ts">
  /*
   * The real SSE connection indicator: `connect()` opens the board's one
   * shared stream (idempotent — `AlarmList` calls it too), and the pulse
   * reflects the status that stream maintains.
   */
  import { onMount } from 'svelte';
  import { connect, connectionStatus } from '../lib/connection.svelte';

  onMount(connect);

  let now = $state(new Date());
  let status = $derived(connectionStatus());

  onMount(() => {
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
    class:bg-ok={status === 'live'}
    class:bg-ink-3={status !== 'live'}
    aria-hidden="true"
  ></span>
  <span>{status === 'live' ? 'live' : 'connecting'}</span>
  <span class="tabular-nums">{clock}</span>
</div>
