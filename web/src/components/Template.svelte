<script lang="ts">
  /**
   * The signature component.
   *
   * A masked template is the most characteristic object in oarfish — it is what
   * gets cached, judged once, and correlated on. So it gets the clearest
   * treatment on the board: the template set large, the parts that change
   * highlighted, and a plain list beneath saying what each part is and how
   * often it varies. No regex on display; the pattern rides on `title` for
   * anyone who wants it.
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

  const slotCount = $derived(pieces.filter((p) => p.kind === 'slot').length);
  const totalSeen = $derived(slots.reduce((sum, s) => sum + s.seen, 0));
</script>

<figure class="sheet m-0">
  <figcaption class="label512">Template</figcaption>
  <div class="tmpl font-mono" aria-label="Masked template">
    {#each pieces as piece}{#if piece.kind === 'text'}{piece.value}{:else}<span
          class="slot">{piece.value}</span>{/if}{/each}
  </div>
  <p class="hint">Highlighted bits change line to line. The rest is fixed.</p>

  {#if slots.length}
    <ul class="vary">
      {#each slots as slot}
        <li title={slot.pattern}>
          <span class="slot small">{slot.name}</span>
          <span>{slot.seen.toLocaleString()} values</span>
        </li>
      {/each}
    </ul>
    <p class="totals font-mono">
      {slotCount} {slotCount === 1 ? 'changing part' : 'changing parts'} · {totalSeen.toLocaleString()} values seen
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
    line-height: 2.2;
    overflow-wrap: break-word;
    color: var(--ui-ink);
  }

  .slot {
    display: inline-block;
    margin: 0 2px;
    padding: 2px 10px;
    border-radius: 999px;
    background: var(--ui-accent-soft);
    color: var(--ui-accent);
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.06em;
    vertical-align: 2px;
    white-space: nowrap;
  }
  .slot.small {
    padding: 1px 9px;
    font-size: 11px;
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
  }

  .totals {
    margin: 12px 0 0;
    font-size: 10.5px;
    letter-spacing: 0.04em;
    color: var(--ui-ink-3);
  }
</style>
