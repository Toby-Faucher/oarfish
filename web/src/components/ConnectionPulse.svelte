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

<div
  class="flex items-center gap-2 rounded-(--radius-ui) border border-line bg-l0 px-2 py-1 font-mono text-[11px] tabular-nums"
  role="status"
  aria-label={status === 'live' ? 'Connected' : 'Connecting'}
>
  <span
    class="size-1.5 rounded-full {status === 'live' ? 'bg-cleared' : 'bg-major'}"
    aria-hidden="true"
  ></span>
  <span class="text-ink-2">{status === 'live' ? 'live' : 'connecting'}</span>
  <span class="text-ink-3">{clock}</span>
</div>
