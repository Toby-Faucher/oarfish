<script lang="ts">
  /*
   * The real SSE connection indicator: `connect()` opens the board's one
   * shared stream (idempotent — `AlarmList` calls it too), and the pulse
   * reflects the status that stream maintains.
   */
  import { onMount } from 'svelte';
  import { connect, connectionStatus } from '../lib/connection.svelte';

  onMount(connect);

  let status = $derived(connectionStatus());
</script>

<div
  class="flex items-center gap-2 rounded-(--radius-ui) border border-line bg-l0 px-2 py-1 font-mono text-[11px] tabular-nums"
  role="status"
  aria-label={status === 'live' ? 'Connected' : 'Connecting'}
>
  <!--
    Neutral dot on purpose: severity hues are reserved for severity, so a
    green dot must never read as "all clear". No clock either: alarm times
    are UTC, and a second local-time clock on screen invites misreads.
  -->
  <span
    class="size-1.5 rounded-full {status === 'live' ? 'bg-ink-2' : 'bg-line-2'}"
    aria-hidden="true"
  ></span>
  <span class="text-ink-2">{status === 'live' ? 'live' : 'connecting'}</span>
</div>
