import React from "react";
import { useTranslation } from "react-i18next";
import { ArrowUp, Square } from "lucide-react";
import type { AgentPanelWorkspaceV1 } from "@/bindings";
import { Textarea } from "@/components/vg/textarea";
import { composerKeys } from "./chatModel";

export interface ChatComposerProps {
  workspace: AgentPanelWorkspaceV1;
  draft: string;
  /** The column focuses this when it opens; nothing else reaches into it. */
  fieldRef: React.RefObject<HTMLTextAreaElement | null>;
  /** A turn is in flight: the field waits and the send glyph becomes a stop. */
  running: boolean;
  /** Nothing would answer, or a command is mid-flight. */
  disabled: boolean;
  onDraftChange: (draft: string) => void;
  onSend: () => void;
  onStop: () => void;
}

/**
 * The field, and the one glyph that sends it.
 *
 * The scope chips moved into the header's menu, so the only thing this band
 * carries is the question being written. Which sandbox is listening is said by
 * the placeholder — "Change a setting" while Configure is chosen — because a
 * label naming a control that is no longer here would be a second copy of a
 * datum the menu already holds.
 *
 * The field is a textarea and not a one-line input for exactly one reason:
 * Shift+Enter has to be able to put a newline in a question. Enter alone sends.
 */
export const ChatComposer: React.FC<ChatComposerProps> = ({
  workspace,
  draft,
  fieldRef,
  running,
  disabled,
  onDraftChange,
  onSend,
  onStop,
}) => {
  const { t } = useTranslation();
  const inert = disabled || running;

  return (
    /* The band at the other end of the column from the header, frosted the
       same way: --surface-raised under Solid, --glass-tint-dense under Glass
       (styles/shell.css). Between the two bands the scrollback keeps the
       page's own colour, so the column reads as a canvas with chrome at its
       ends rather than as one flat strip. */
    <form
      data-slot="chat-composer"
      className="flex flex-none flex-col border-t border-gray-alpha-400 bg-surface-raised p-3"
      onSubmit={(event) => {
        event.preventDefault();
        onSend();
      }}
    >
      {/* `items-end` so a question grown to three lines pushes the field up and
          leaves the send glyph on the baseline it started on. */}
      <div className="flex items-end gap-1.5 rounded-[20px] border border-gray-alpha-400 bg-background-100 p-1 ps-3 focus-within:border-gray-alpha-600">
        <Textarea
          ref={fieldRef}
          rows={1}
          className="max-h-28 min-h-0 flex-1 resize-none rounded-none border-0 bg-transparent px-0 py-1.5 text-[14px] leading-[21px] focus-visible:ring-0 md:text-[14px]"
          value={draft}
          onChange={(event) => onDraftChange(event.target.value)}
          onKeyDown={composerKeys(onSend)}
          placeholder={
            workspace === "sona_config"
              ? t("chat.placeholderConfig")
              : t("chat.placeholder")
          }
          aria-label={t("chat.inputLabel")}
          disabled={inert}
        />
        {running ? (
          /* Quiet on purpose: stopping is a retreat, not the thing this row is
             for, so it takes the outline the send glyph fills. */
          <button
            type="button"
            data-slot="chat-stop"
            onClick={onStop}
            disabled={disabled}
            aria-label={t("chat.stop")}
            className="grid size-7 flex-none place-items-center rounded-full border border-gray-alpha-400 text-gray-900 transition-colors hover:text-gray-1000 disabled:pointer-events-none disabled:opacity-50 motion-reduce:transition-none"
          >
            <Square aria-hidden="true" className="size-3 fill-current" />
          </button>
        ) : (
          <button
            type="submit"
            data-slot="chat-send"
            disabled={disabled || draft.trim() === ""}
            aria-label={t("chat.send")}
            className="grid size-7 flex-none place-items-center rounded-full border border-transparent bg-gray-1000 text-background-100 transition-colors enabled:hover:bg-gray-900 disabled:border-gray-alpha-400 disabled:bg-transparent disabled:text-gray-700 disabled:opacity-100 disabled:pointer-events-none motion-reduce:transition-none"
          >
            <ArrowUp aria-hidden="true" className="size-4" />
          </button>
        )}
      </div>
    </form>
  );
};
