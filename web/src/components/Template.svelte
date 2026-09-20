<script lang="ts">
  /**
   * The signature component.
   *
   * A masked template is the most characteristic object in oarfish — it is what
   * gets cached, judged once, and correlated on. So it gets the clearest
   * treatment on the board: the template set large, the parts that change
   * boxed with leader ticks, and a dimension list beneath saying what each
   * part is and, where the detail payload provides it, what it matched and
   * how often it varies.
   *
   * Static markup, no JavaScript.
   *
   * This is the one place the design is allowed to be loud. If a second
   * component starts competing with it, cut something.
   */
  import type { Slot } from '../lib/bindings/oarfish';

  interface Props {
    /** Template text with `<VAR:NAME>` placeholders, straight from Drain. */
    template: string;
    /**
     * Enrichment per slot name: the regex that matched and how many values
     * were seen. Empty until the detail endpoint carries it — the slot list
     * below is derived from the template text itself, so the signature
     * renders at full power either way.
     */
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

  type SlotRow = { name: string; pattern: string | null; seen: number | null; uses: number };

  /** One row per distinct placeholder, in first-appearance order. */
  const vary = $derived.by((): SlotRow[] => {
    const uses = new Map<string, number>();
    const order: string[] = [];
    for (const piece of pieces) {
      if (piece.kind !== 'slot') continue;
      uses.set(piece.value, (uses.get(piece.value) ?? 0) + 1);
      if (!order.includes(piece.value)) order.push(piece.value);
    }
    return order.map((name) => {
      const known = slots.find((slot) => slot.name === name);
      return {
        name,
        pattern: known?.pattern ?? null,
        seen: known?.seen ?? null,
        uses: uses.get(name) ?? 1,
      };
    });
  });

  const slotCount = $derived(vary.length);
  const totalSeen = $derived(slots.reduce((sum, s) => sum + s.seen, 0));
</script>

<figure class="sheet m-0">
  <figcaption class="label512">Template</figcaption>
  <div class="tmpl font-mono" aria-label="Masked template">
    {#each pieces as piece}{#if piece.kind === 'text'}{piece.value}{:else}<span
          class="slot">{piece.value}</span>{/if}{/each}
  </div>
  <p class="hint">Boxed bits change line to line. The rest is fixed.</p>

  {#if vary.length}
    <ul class="vary">
      {#each vary as row}
        <li>
          <span class="tick" aria-hidden="true"></span>
          <span class="slot small">{row.name}</span>
          {#if row.pattern}
            <code class="pattern">{row.pattern}</code>
          {/if}
          <span class="meta">
            {#if row.seen !== null}{row.seen.toLocaleString()} values · {/if}{row.uses}× in template
          </span>
        </li>
      {/each}
    </ul>
    <p class="totals font-mono">
      {slotCount} {slotCount === 1 ? 'changing part' : 'changing parts'}{#if slots.length}
        · {totalSeen.toLocaleString()} values seen{/if}
    </p>
  {/if}
</figure>

<style>
  .sheet {
    border: 1px solid var(--ui-line);
    border-radius: 12px;
    background: var(--ui-l0);
    padding: 16px 18px 14px;
  }

  .label512 {
    font-family: var(--font-cond);
    font-weight: 700;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ui-ink-3);
  }

  .tmpl {
    margin-top: 8px;
    font-size: 14.5px;
    line-height: 2.4;
    overflow-wrap: break-word;
    color: var(--ui-ink);
  }

  /*
   * Boxed, neutral, never the accent: --ui-accent means interactive, and
   * these boxes are read, not pressed. The tick below each box is the
   * leader tying it to the dimension list underneath.
   */
  .slot {
    position: relative;
    display: inline-block;
    margin: 0 2px;
    padding: 1px 8px;
    border: 1px solid var(--ui-line-2);
    border-radius: 3px;
    background: var(--ui-l1);
    color: var(--ui-ink);
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.06em;
    vertical-align: 2px;
    white-space: nowrap;
  }
  .slot::after {
    content: '';
    position: absolute;
    left: 50%;
    top: 100%;
    width: 1px;
    height: 5px;
    background: var(--ui-line-2);
  }
  .slot.small {
    padding: 0 7px;
    font-size: 11px;
  }
  .slot.small::after {
    display: none;
  }

  .hint {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--ui-ink-3);
  }

  .vary {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 16px;
    margin: 14px 0 0;
    padding: 14px 0 0;
    border-top: 1px solid var(--ui-line);
    list-style: none;
  }
  .vary li {
    display: flex;
    align-items: center;
    gap: 8px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ui-ink-2);
    max-width: 100%;
  }
  .tick {
    flex: none;
    width: 1px;
    height: 10px;
    background: var(--ui-line-2);
  }
  .pattern {
    font-size: 11px;
    color: var(--ui-ink-2);
    overflow-wrap: anywhere;
  }
  .meta {
    flex: none;
    font-size: 11px;
    color: var(--ui-ink-3);
  }

  .totals {
    margin: 12px 0 0;
    font-size: 10.5px;
    letter-spacing: 0.04em;
    color: var(--ui-ink-3);
  }
</style>
