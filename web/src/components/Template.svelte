<script lang="ts">
  /**
   * The signature component.
   *
   * A masked template is the most characteristic object in oarfish — it is what
   * gets cached, judged once, and correlated on. So it is drawn the way a
   * schematic draws a dimensioned part: the variable slots are measured slots,
   * and the dimension lines beneath report what was measured and how often.
   *
   * This is the one place the design is allowed to be loud. If a second
   * component starts competing with it, cut something.
   */
  export interface Slot {
    name: string;
    pattern: string;
    seen: number;
  }

  interface Props {
    /** Template text with `<VAR:NAME>` placeholders, straight from Drain. */
    template: string;
    slots?: Slot[];
  }

  let { template, slots = [] }: Props = $props();

  type Piece = { kind: 'text'; value: string } | { kind: 'slot'; value: string };

  const pieces = $derived.by((): Piece[] => {
    const out: Piece[] = [];
    const re = /<VAR:([A-Z0-9_]+)>/g;
    let last = 0;
    let m: RegExpExecArray | null;

    while ((m = re.exec(template)) !== null) {
      if (m.index > last) out.push({ kind: 'text', value: template.slice(last, m.index) });
      out.push({ kind: 'slot', value: m[1] });
      last = m.index + m[0].length;
    }
    if (last < template.length) out.push({ kind: 'text', value: template.slice(last) });
    return out;
  });
</script>

<div class="font-mono text-[13.5px] leading-[2.4] break-words text-ink-2">
  {#each pieces as piece}{#if piece.kind === 'text'}{piece.value}{:else}<span
        class="slot">{piece.value}</span>{/if}{/each}
</div>

{#if slots.length}
  <dl class="mt-5 flex flex-col gap-2.5">
    {#each slots as slot}
      <div class="grid grid-cols-[64px_56px_1fr_auto] items-center gap-3 font-mono text-[11px]">
        <span class="dim" aria-hidden="true"></span>
        <dt class="tracking-[0.08em] text-accent">{slot.name}</dt>
        <dd class="m-0 overflow-x-auto whitespace-nowrap text-ink-3">{slot.pattern}</dd>
        <dd class="m-0 tabular-nums text-ink-2">{slot.seen.toLocaleString()} values</dd>
      </div>
    {/each}
  </dl>
{/if}

<style>
  /* A measured slot: boxed, with leader ticks reaching out to the text either side. */
  .slot {
    display: inline-block;
    position: relative;
    margin: 0 1px;
    padding: 1px 7px;
    border: 1px solid var(--ui-accent);
    border-radius: 2px;
    background: var(--ui-accent-soft);
    color: var(--ui-accent);
    font-size: 11.5px;
    font-weight: 500;
    letter-spacing: 0.07em;
    vertical-align: 1px;
  }

  .slot::before,
  .slot::after {
    content: '';
    position: absolute;
    top: 50%;
    width: 3px;
    height: 1px;
    background: var(--ui-accent);
    opacity: 0.55;
  }
  .slot::before { left: -4px; }
  .slot::after { right: -4px; }

  /* Dimension line: a promise that somebody measured something. */
  .dim {
    position: relative;
    height: 1px;
    background: var(--ui-line-2);
  }
  .dim::before,
  .dim::after {
    content: '';
    position: absolute;
    top: -3.5px;
    width: 1px;
    height: 8px;
    background: var(--ui-line-2);
  }
  .dim::before { left: 0; }
  .dim::after { right: 0; }
</style>
