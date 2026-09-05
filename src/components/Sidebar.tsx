import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { MessageSquare, Search } from "lucide-react";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/vg/tooltip";
import { destinationIcons } from "@/lib/navIcons";
import { cn } from "@/lib/cn";
import { getLanguageDirection } from "@/lib/utils/rtl";
import { SonaMark } from "./icons/SonaMark";
import {
  RAIL_SECTIONS,
  SECTIONS_CONFIG,
  type SidebarSection,
} from "./sidebarSections";
import { useOsType } from "@/hooks/useOsType";

export interface SidebarProps {
  /**
   * Whether the chat column has the width. Collapsed, the rail is 48pt of
   * glyphs and the page keeps 512pt beside the chat; expanded it is the
   * 220pt rail with every destination named.
   */
  collapsed: boolean;
  currentSection: SidebarSection;
  onSectionChange: (section: SidebarSection) => void;
  onOpenCommand: () => void;
  /**
   * The agent's two settings. `agent_panel_enabled` off means the chat row
   * does not exist; `agent_panel_paired` off means no relay would answer a
   * turn, so the row is inert and says why.
   */
  agentPanel: { enabled: boolean; paired: boolean };
  /**
   * Whether the chat column is showing. The row reflects it — the column is a
   * region this button discloses, not a destination — and never claims
   * `aria-current` for it.
   */
  chatOpen: boolean;
  /** Presses only ever open: closing belongs to the column's X and to Esc. */
  onOpenChat: () => void;
  /**
   * The short-lived outgoing rail uses the same complete rendering as the
   * active one, but its separate slot keeps geometry readers on the structural
   * rail and gives the shell CSS a precise crossfade target.
   */
  dataSlot?: "sidebar" | "sidebar-ghost";
  /** Makes the outgoing visual rail unavailable to pointer and assistive use. */
  decorative?: boolean;
  className?: string;
}

/* The rail's rows come from the section registry. Modes and Models stay
 * available through the command palette without occupying permanent rail
 * space. Each visible row compares against the shell's current section. */

/* The wordmark is the product's name, not copy; it never localizes. */
const WORDMARK = "Sona";

/* One row, in every state it has. Selection is a wash at the control radius,
 * not a bordered plate: a border on the selected row alone puts a second
 * hairline inside a rail that already has one, and the wash says the same
 * thing with nothing drawn. Focus is the shared bronze ring. */
const NAV_ROW =
  "flex items-center rounded-md text-[14px] leading-[21px] whitespace-nowrap text-gray-900 transition-colors hover:bg-gray-alpha-100 hover:text-gray-1000 focus-visible:ring-2 focus-visible:ring-focus-ring focus-visible:outline-none motion-reduce:transition-none";

/* The selected row: the same wash, and its words in the primary ink at medium.
 * Weight is what a reader sees first at this size — the wash alone measured as
 * a 4% step off the rail. */
const NAV_ROW_CURRENT = "bg-gray-alpha-200 font-medium text-gray-1000";

/* A destination glyph, at the em box of the 14px label beside it. Pinned
 * in px, not `size-3.5`: base.css puts the root at 14px, so a rem box lands on
 * half device pixels and blurs. */
const NAV_ICON = "size-[14px] flex-none";

/* The two shapes a row takes. The glyph square is the named row with its words
 * clipped off, not a second navigation: same 32pt height, same radius, same
 * selected border and fill, same focus ring. Pinned in px because base.css
 * puts the root at 14px, so `size-8` would be a 28pt box on a 32pt row.
 *
 * 8 + 32 + 8 is the rail's 48. */
const NAV_ROW_NAMED = "h-[32px] gap-2 px-[10px] text-start";
const NAV_ROW_GLYPH = "size-[32px] flex-none justify-center";

/* The chat frame owns the shell's one transition. The rail and the content
 * pane take their final widths in the press frame, which is what stops every
 * item in the flexing page from reflowing for 150ms; `transition-none` on the
 * rail makes a later broad utility unable to accidentally turn it back into a
 * second clock. */

/** Moves focus between sibling nav rows on arrow keys. Tab order keeps every
 * row, so this is an addition to normal tabbing, not a replacement. Up/Down
 * because the list is vertical; the keys need no RTL mirroring. */
const useArrowNavigation = () => {
  const groupRef = useRef<HTMLElement>(null);

  const onKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    const step =
      event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
    if (step === 0) return;

    const buttons = Array.from(
      groupRef.current?.querySelectorAll<HTMLButtonElement>("button") ?? [],
    );
    const current = buttons.findIndex(
      (button) => button === document.activeElement,
    );
    if (current === -1) return;

    event.preventDefault();
    buttons[(current + step + buttons.length) % buttons.length].focus();
  };

  return { groupRef, onKeyDown };
};

interface RowNameProps {
  /** Whether the sentence is worth showing at all. */
  show: boolean;
  /** Radix sides are physical; the rail is not. */
  side: "left" | "right";
  name: string;
  children: React.ReactElement;
}

/**
 * What a row would say if it could: its name while the rail is glyphs, or the
 * reason it is inert.
 *
 * Radix opens on focus as well as hover and points the trigger's
 * `aria-describedby` at the sentence while it is open, so a collapsed rail
 * stays readable by keyboard rather than pointer-only. Each row keeps its
 * `aria-label` either way: the tooltip is how a sighted reader recovers the
 * word, not how the row is named.
 */
const RowName: React.FC<RowNameProps> = ({ show, side, name, children }) =>
  show ? (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side={side}>{name}</TooltipContent>
    </Tooltip>
  ) : (
    children
  );

export const Sidebar: React.FC<SidebarProps> = ({
  collapsed,
  currentSection,
  onSectionChange,
  onOpenCommand,
  agentPanel,
  chatOpen,
  onOpenChat,
  dataSlot = "sidebar",
  decorative = false,
  className,
}) => {
  const { t, i18n } = useTranslation();
  const nav = useArrowNavigation();
  const isMac = useOsType() === "macos";
  const side = getLanguageDirection(i18n.language) === "rtl" ? "left" : "right";
  const chatRowRef = useRef<HTMLButtonElement>(null);
  const chatWasOpen = useRef(false);

  /* Closing the column takes the element under the reader's focus off screen:
   * the column's X and Escape are the only ways out, and both live inside it.
   * Focus comes back to the row the press started from rather than falling to
   * the body. Only after the column has actually been open, so a fresh window
   * does not steal focus from whatever the route put it on — and never from
   * the outgoing visual rail, which is `inert` and is not the real one. */
  useEffect(() => {
    if (decorative) return;
    if (chatWasOpen.current && !chatOpen) chatRowRef.current?.focus();
    chatWasOpen.current = chatOpen;
  }, [chatOpen, decorative]);

  return (
    /* The rail carries the page's own surface and is closed by a hairline on
       its content edge — same fill on both sides of that line, which is what
       makes the line the whole separation. `glass-surface` is inert until the
       Material setting is Glass and the native vibrancy view is actually
       behind the window; see styles/primitives.css. The sidebar is chrome with
       the desktop behind it, which is exactly the layer the glass ruling sends
       translucent.

       Collapsed it is the same rail at 48pt: the destinations, in the same
       order, in the same states, with their words in tooltips instead of
       beside their glyphs. `overflow-hidden` keeps the named form from leaking
       across the glyph form during the one press-frame width swap. */
    <aside
      data-slot={dataSlot}
      aria-hidden={decorative ? true : undefined}
      inert={decorative}
      className={cn(
        "glass-surface flex min-h-0 flex-none flex-col overflow-hidden border-e border-gray-alpha-400 bg-background-200 pb-[10px] transition-none",
        /* Chrome that carries 14px labels takes the dense tint rather than
           the airy one: over a bright wallpaper the 0.70 tint left secondary
           text at 3.1:1, and the dense step measures 5.5:1 on the same
           backdrop. The token is read by the material rule in
           primitives.css, so setting it here is the whole opt-in. */
        "[--glass-tint:var(--glass-tint-dense)]",
        collapsed ? "w-[48px] px-[8px]" : "w-[220px] px-[10px]",
        className,
      )}
    >
      {/* Clearance for the overlay title bar's traffic lights (the window is
          TitleBarStyle::Overlay with a hidden title); the spacer stays a live
          drag handle, like the brand row under it. Other platforms keep their
          native title bar and need only a breath. */}
      <div
        className={isMac ? "h-[38px] flex-none" : "h-2 flex-none"}
        data-tauri-drag-region
      />

      {/* Collapsed, the mark is the brand: 48pt has no room for a wordmark, and
          a truncated product name is worse than none. */}
      <div
        className={cn(
          "mb-4 flex min-h-8 flex-none items-center text-gray-1000",
          collapsed ? "justify-center" : "gap-2 px-[8px]",
        )}
        data-tauri-drag-region
      >
        <SonaMark width={18} height={18} className="flex-none" />
        {!collapsed && (
          /* Pinned like the magnifier: `text-sm` is 12.25px at this app's 14px
             root, which would have shrunk the wordmark from the 14px the
             deleted `.app-sidebar-wordmark` rule set. */
          <span
            className="text-[14px] leading-[20px] font-semibold tracking-[-0.01em] whitespace-nowrap"
            data-tauri-drag-region
          >
            {WORDMARK}
          </span>
        )}
      </div>

      {/* The rail's two actions, above the gap that separates them from the
          destinations under it. Neither row is a place: Search opens the
          command palette and Chat opens the chat column, so both sit outside
          the `nav` landmark and outside its arrow ring, and neither can ever
          take `aria-current`. Grouping them is what keeps a door from reading
          as a page by sitting between two of them.

          The group owns the 12px gap rather than either row, so the spacing
          below Search is the same whether the chat row is there or the agent
          is switched off. */}
      <div className="mb-3 flex flex-none flex-col gap-0.5">
        {/* A search field that is really a button: it opens the command
            palette, so it carries the shortcut that does the same thing
            instead of a caret. It used to keep a plate — a raised fill and a
            hairline — and the plate was plywood: background-100 is a 4% step
            off this rail and the hairline is white at 14%, so on this surface
            neither one was visible. An element that is not doing its job gets
            subtracted rather than strengthened, so the row is flat like the
            nav rows under it and the keycaps are what mark it as the chord's
            affordance. The global Cmd/Ctrl+K binding lives in App.tsx.

            Collapsed it keeps the magnifier and loses the keycap: the chord is
            a thing the row was advertising, and there is no width to advertise
            it in. The binding itself is unaffected. */}
        <RowName show={collapsed} side={side} name={t("commandPalette.open")}>
          <button
            type="button"
            className={cn(NAV_ROW, collapsed ? NAV_ROW_GLYPH : NAV_ROW_NAMED)}
            aria-label={t("commandPalette.open")}
            onClick={onOpenCommand}
          >
            <Search className={NAV_ICON} aria-hidden="true" />
            {!collapsed && (
              <>
                <span className="min-w-0 flex-1 truncate text-start text-[14px]">
                  {t("commandPalette.open")}
                </span>
                {/* The chord, in Meta type with tabular digits, not a keycap:
                    the plate this row used to wear was already subtracted for
                    being invisible on this surface, and a boxed chip beside a
                    flat row was the last piece of that plate. */}
                <span
                  aria-hidden="true"
                  className="flex-none text-[13px] leading-[18px] tabular-nums text-gray-800"
                >
                  {isMac ? "\u2318 K" : "Ctrl K"}
                </span>
              </>
            )}
          </button>
        </RowName>

        {/* The way into the chat column, and the reason there is no longer a
            floating pill over the pane: every page puts its own primary action
            at the top right of its title row, so a control the shell parked in
            that corner covered whichever one the route happened to draw —
            Library's "Import audio" among them. The rail is the one surface in
            this window that no page draws into.

            It carries the pill's three states unchanged. Off by setting is
            nothing at all: a disabled row pointing at a switch the reader
            turned off themselves is noise, and Settings is where it comes
            back. Unpaired stays visible and inert with the reason in the
            tooltip, because hiding it would make the fix undiscoverable.

            `aria-expanded` and the pressed wash, never `aria-current`: the
            column is a region this button discloses, not a sixth destination.
            The press only ever opens — the column's X and Escape own the way
            back out — so pressing it while the column is up is the same
            request again and changes nothing. */}
        {agentPanel.enabled && (
          <RowName
            show={collapsed || !agentPanel.paired}
            side={side}
            name={agentPanel.paired ? t("chat.open") : t("chat.unpaired")}
          >
            <button
              type="button"
              ref={chatRowRef}
              data-slot="chat-rail-row"
              /* The label reads "Chat", which does not say chat with what. The
                 accessible name does. */
              aria-label={t("chat.label")}
              aria-expanded={agentPanel.paired ? chatOpen : undefined}
              /* `aria-disabled`, not `disabled`: the reason it is inert lives
                 in the tooltip above, and a `disabled` button takes neither
                 hover nor focus, so it would be the one state that cannot
                 reach its own explanation. Dimmed type rather than opacity,
                 like every disabled settings row. */
              aria-disabled={agentPanel.paired ? undefined : true}
              onClick={agentPanel.paired ? onOpenChat : undefined}
              className={cn(
                NAV_ROW,
                collapsed ? NAV_ROW_GLYPH : NAV_ROW_NAMED,
                chatOpen && NAV_ROW_CURRENT,
                !agentPanel.paired &&
                  "text-gray-800 hover:bg-transparent hover:text-gray-800",
              )}
            >
              <MessageSquare aria-hidden="true" className={NAV_ICON} />
              {!collapsed && t("chat.open")}
            </button>
          </RowName>
        )}
      </div>

      {/* The destinations. The nav takes the rail's remaining height so the
          last row can sit against the bottom edge rather than under People:
          Settings is the one row nobody navigates to while working, and the
          four above it are where the reading happens. Declaration order in
          the registry is still the order — the gap is `mt-auto` on one row,
          not a second list. */}
      <nav
        ref={nav.groupRef}
        onKeyDown={nav.onKeyDown}
        className="flex min-h-0 flex-1 flex-col gap-0.5"
        aria-label={t("sidebar.navigation")}
      >
        {RAIL_SECTIONS.map((section) => {
          const DestinationIcon = destinationIcons[section];
          const label = t(SECTIONS_CONFIG[section].labelKey);
          const current = section === currentSection;
          return (
            <RowName key={section} show={collapsed} side={side} name={label}>
              <button
                type="button"
                aria-current={current ? "page" : undefined}
                aria-label={label}
                className={cn(
                  NAV_ROW,
                  collapsed ? NAV_ROW_GLYPH : NAV_ROW_NAMED,
                  current && NAV_ROW_CURRENT,
                  section === "settings" && "mt-auto",
                )}
                onClick={() => onSectionChange(section)}
              >
                <DestinationIcon aria-hidden="true" className={NAV_ICON} />
                {!collapsed && label}
              </button>
            </RowName>
          );
        })}
      </nav>
    </aside>
  );
};
