import React from "react";
import { useTranslation } from "react-i18next";
import { Toaster as SonnerToaster } from "sonner";
import { getLanguageDirection } from "@/lib/utils/rtl";

/* The app's single toast surface. Mount once at the shell root; raise
 * messages with `toast` from sonner.
 *
 * Copy rules, because a toast is its sentence:
 *   - one sentence, and never a title with a restatement under it: two type
 *     sizes saying one thing is the shape this surface used to have, and the
 *     sentence was always the one that said it. A second line is for what the
 *     first cannot carry - a backend failure's own words, or what a decision
 *     would do - never for saying the first line again. A short form belongs
 *     on the recording HUD, where two words is all the pill can hold;
 *   - pick the method by how the event was experienced, not by status code —
 *     a cancel is .message(), a partial success is .warning();
 *   - completions are "{Noun} {past-participle}" — "Snippet saved", never
 *     "Snippet saved successfully";
 *   - an error names the cause, then the way out — "Couldn't transcribe. Try
 *     again.";
 *   - sentence case, and no trailing period on a single-sentence toast.
 *
 * Raised as a raised surface rather than a card: it floats over the page, so
 * it earns both the hairline and the shadow. */
export const Toaster: React.FC = () => {
  const { i18n } = useTranslation();
  /* Bottom-end. Sonner only knows physical corners, so the trailing one is
   * chosen from the document direction the language sets. */
  const position =
    getLanguageDirection(i18n.language) === "rtl"
      ? "bottom-left"
      : "bottom-right";
  return (
    <SonnerToaster
      theme="system"
      position={position}
      toastOptions={{
        unstyled: true,
        classNames: {
          /* A frosted card: `glass-surface` under Glass, --surface-raised
             under Solid, --radius-panel, one hairline, one soft shadow — the
             same object the palette and the menus are. `toast-surface` is what
             styles/popups.css hangs the app's popup motion on, since sonner's
             open state is `data-mounted` rather than a Radix `data-state`.

             The tint is the dense one. `.glass-surface` paints
             `background: var(--glass-tint)`, so overriding the variable on the
             element is what asks for a denser frost without fighting an
             unlayered rule. This is the one popup that carries an action label
             in the scarce accent, and the brief's rule for a glass tint under
             type is to take the dense step when in doubt. */
          toast:
            "glass-surface toast-surface flex items-center gap-3 rounded-panel border border-gray-alpha-400 bg-surface-raised px-4 py-3 text-[14px] leading-[21px] shadow-[var(--shadow-popover)] [--glass-tint:var(--glass-tint-dense)]",
          title: "font-medium text-text-primary",
          /* The second line, when there is one: a failed model load prints the
             backend's reason under it, and the detection prompt puts what Sona
             would do under the app it saw. `unstyled` turns off sonner's own
             `[data-description]` rule too, so without this the second line
             reads at the same weight as the first. Secondary type is the same
             13/18 gray the rows use. */
          description: "text-[13px] leading-[18px] text-gray-900",
          /* Ghost text buttons, not filled ones. A toast is one line that
             leaves on its own; a filled bronze button on it competes with
             whatever the reader was actually doing, and two filled buttons
             beside each other — which is what the detection prompt raises —
             is the "every action weighted the same" tell. The accent is the
             one that acts, gray is the one that dismisses, and the timing
             tokens are gone from here because the toast's own transition now
             carries them. */
          /* `text-accent-strong`, not `text-accent`: app/globals.css points
             Tailwind's `accent` at shadcn's meaning — the hover wash on a menu
             row, --gray-a-200 — and the bronze lives on `accent-strong`. This
             surface shipped `bg-accent text-on-accent`, which was white type
             on a 6% ink wash: an invisible label on an invisible fill. */
          actionButton:
            "min-h-7 cursor-pointer rounded-control px-2 text-[14px] font-medium whitespace-nowrap text-accent-strong transition-colors hover:bg-accent-soft",
          cancelButton:
            "min-h-7 cursor-pointer rounded-control px-2 text-[14px] whitespace-nowrap text-gray-900 transition-colors hover:bg-gray-alpha-200 hover:text-gray-1000",
        },
      }}
    />
  );
};
