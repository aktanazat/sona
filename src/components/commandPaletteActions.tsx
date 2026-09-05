export type CommandPaletteGroup = "navigation" | "actions";

/* A row of the palette is its words. It carried a `LucideIcon` too, and every
 * one of those glyphs sat beside a label that already said the same thing, so
 * the field is gone rather than merely unrendered: a registry of marks nothing
 * draws is the next surface's temptation. */
export interface CommandPaletteAction {
  id: string;
  group: CommandPaletteGroup;
  label: string;
  run: () => void;
}

/**
 * Whether one keydown is the chord that summons the palette.
 *
 * `repeat` is the whole reason this is a named predicate rather than three
 * conditions inline in the listener. The chord toggles, and a held key
 * auto-repeats keydown at the OS repeat rate — so without this the palette
 * opened and closed dozens of times a second for as long as the chord was
 * held. That was the flicker. `GlobalShortcutInput` already dropped repeats
 * for the same reason; this is the same rule at the other keyboard entry
 * point.
 *
 * The parameter is the four fields it reads, not a `KeyboardEvent`, so the
 * rule is provable without a DOM.
 */
export const isCommandPaletteChord = (
  event: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "repeat">,
): boolean =>
  !event.repeat &&
  (event.metaKey || event.ctrlKey) &&
  event.key.toLowerCase() === "k";

export interface CommandPaletteSection {
  group: CommandPaletteGroup;
  items: CommandPaletteAction[];
}

/* Destinations before verbs: the palette is navigation first, and the order is
 * fixed here rather than at the call site so the registry can stay a flat
 * list. cmdk hides a group whose every item is filtered out, so this only has
 * to drop a group nothing ever contributed to — the agent action when the
 * agent panel is off. */
const GROUP_ORDER: readonly CommandPaletteGroup[] = ["navigation", "actions"];

export const groupPaletteActions = (
  actions: readonly CommandPaletteAction[],
): CommandPaletteSection[] =>
  GROUP_ORDER.map((group) => ({
    group,
    items: actions.filter((action) => action.group === group),
  })).filter((section) => section.items.length > 0);
