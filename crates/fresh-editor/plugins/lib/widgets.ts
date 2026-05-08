/**
 * Plugin widget library — declarative UI for Fresh plugins.
 *
 * Plugins describe panel content as a `WidgetSpec` tree. The host owns
 * rendering, theming, and (in later phases) hit-testing, focus, and
 * keymaps. This module provides:
 *
 *   - Type re-exports from the generated `fresh.d.ts` so plugins import
 *     `WidgetSpec` / `HintEntry` from one place.
 *   - Builder helpers (`row`, `col`, `hintBar`, `raw`) that produce the
 *     correct discriminated-union shape.
 *   - A `WidgetPanel` class that wraps the
 *     `mountWidgetPanel` / `updateWidgetPanel` / `unmountWidgetPanel`
 *     IPC trio with mount-once-then-update semantics.
 *   - `parseHintString(s)` — parses the legacy `Tab:section  Esc:close`
 *     string format used by today's plugin i18n bundles into
 *     `HintEntry[]`.
 *
 * See `docs/internal/plugin-widget-library-design.md`.
 *
 * @example
 *   import { WidgetPanel, hintBar, col, raw, parseHintString } from "./lib/widgets.ts";
 *
 *   const panel = new WidgetPanel(bufferId);
 *   panel.set(col(
 *     raw(myExistingEntries),
 *     hintBar(parseHintString(editor.t("panel.help"))),
 *   ));
 *   // …later, on every state change:
 *   panel.set(col(raw(newEntries), hintBar(myHints)));
 *   // …on close:
 *   panel.unmount();
 */

/// <reference path="./fresh.d.ts" />

// `fresh.d.ts` declares HintEntry / WidgetSpec / TextPropertyEntry as
// ambient globals (it is not an ES module). Re-export the relevant
// type names locally so plugin code can write
// `import type { WidgetSpec } from "./lib/widgets.ts"` without dipping
// into the ambient namespace directly.
export type WidgetSpec = globalThis.WidgetSpec;
export type HintEntry = globalThis.HintEntry;
type TextPropertyEntry = globalThis.TextPropertyEntry;

// =============================================================================
// Builder helpers — preferred over hand-writing `{ kind: "row", ... }`.
// =============================================================================

/** Horizontal layout. Children laid out left-to-right; inline-sized
 * children collapse into a single line. See §3 of the design doc. */
export function row(...children: WidgetSpec[]): WidgetSpec {
  return { kind: "row", children };
}

/** Vertical layout. Children stacked top-to-bottom. */
export function col(...children: WidgetSpec[]): WidgetSpec {
  return { kind: "col", children };
}

/** Keyboard-hint footer. Renders `<keys> <label>` per entry, with the
 * keys portion styled by the `ui.help_key_fg` theme key.
 *
 * Replaces the per-plugin hand-rolled help row. */
export function hintBar(entries: HintEntry[]): WidgetSpec {
  return { kind: "hintBar", entries };
}

/** Imperative-virtual-buffer escape hatch. Wraps an existing
 * `TextPropertyEntry[]` (the same shape `setVirtualBufferContent`
 * already accepts) so a plugin can migrate its panel one widget at a
 * time. */
export function raw(entries: TextPropertyEntry[]): WidgetSpec {
  return { kind: "raw", entries };
}

// =============================================================================
// HintEntry parsing — for the legacy `Tab:section  Esc:close` format
// shipped in existing plugin i18n bundles.
// =============================================================================

/** Parse a hint string of the form `<keys>:<label>  <keys>:<label> ...`.
 *
 * The separator between entries defaults to two-or-more spaces (matching
 * what existing i18n bundles use). The separator between keys and label
 * within an entry is a colon.
 *
 * Empty input yields an empty array. Entries without a colon are
 * preserved with empty label. */
export function parseHintString(
  s: string,
  options?: { entrySep?: RegExp; keyLabelSep?: string },
): HintEntry[] {
  if (!s) return [];
  const entrySep = options?.entrySep ?? /\s{2,}/;
  const keyLabelSep = options?.keyLabelSep ?? ":";
  const parts = s.split(entrySep).filter((p) => p.length > 0);
  return parts.map((part) => {
    const idx = part.indexOf(keyLabelSep);
    if (idx < 0) {
      return { keys: part, label: "" };
    }
    return {
      keys: part.slice(0, idx).trim(),
      label: part.slice(idx + keyLabelSep.length).trim(),
    };
  });
}

// =============================================================================
// WidgetPanel — mount-once-update-many wrapper around the IPC trio.
// =============================================================================

/** A handle to a mounted widget panel. Construct one per virtual
 * buffer that should host widget-rendered content; call `set(spec)`
 * on every render; call `unmount()` when the buffer is closed.
 *
 * The first `set()` issues `mountWidgetPanel`; subsequent calls
 * issue `updateWidgetPanel`. Idempotent re-mount is guaranteed by the
 * host (see `WidgetRegistry::mount`). */
export class WidgetPanel {
  private mounted = false;
  private readonly panelId: number;
  private readonly bufferId: number;

  constructor(bufferId: number, panelId?: number) {
    this.bufferId = bufferId;
    this.panelId = panelId ?? allocatePanelId();
  }

  /** Returns the plugin-allocated panel id, useful for routing
   * widget events back through `editor.on("widget_event", ...)`. */
  id(): number {
    return this.panelId;
  }

  /** Render or re-render the panel against the given spec.
   * Cheap to call on every state change; the host reconciles. */
  set(spec: WidgetSpec): boolean {
    // deno-lint-ignore no-explicit-any
    const editor = (globalThis as any).editor;
    if (!this.mounted) {
      this.mounted = true;
      return editor.mountWidgetPanel(this.panelId, this.bufferId, spec);
    }
    return editor.updateWidgetPanel(this.panelId, spec);
  }

  /** Tear down the panel. The plugin retains ownership of the
   * underlying virtual buffer. Subsequent `set()` calls re-mount. */
  unmount(): boolean {
    if (!this.mounted) return true;
    this.mounted = false;
    // deno-lint-ignore no-explicit-any
    const editor = (globalThis as any).editor;
    return editor.unmountWidgetPanel(this.panelId);
  }
}

// =============================================================================
// Panel-id allocation. Plugin-side counter; need only be unique per
// plugin instance (the host doesn't interpret the value).
// =============================================================================

let nextPanelId = 1;
function allocatePanelId(): number {
  // Bias high so plugin-allocated ids don't collide with the
  // editor's internal panel-id space if it ever uses small ints.
  const id = nextPanelId++;
  return 0x1000_0000 + id;
}
