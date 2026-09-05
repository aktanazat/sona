import React from "react";
import { Check, Copy, MoreHorizontal, RotateCcw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/vg/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import type { HistoryRowAction } from "./historyRowActions";

interface HistoryRowControlsProps {
  /** The operations that stay behind the menu: correct, save, process again. */
  menuActions: HistoryRowAction[];
  hasText: boolean;
  busy: boolean;
  /** True for the two seconds after a copy, while the check mark shows. */
  showCopied: boolean;
  onCopy: () => void;
  onRetranscribe: () => void;
  onDelete: () => void;
}

/* The expanded row's action bar. It exists only while the row is open, which is
 * what buys the collapsed log its quiet: two named buttons for the two things
 * you open a recording to do — copy its words, transcribe it again — and one
 * menu for everything that changes or removes the entry.
 *
 * Delete is in that menu rather than on the bar because the Library is a
 * reading surface, and a reading surface carries no visible destructive
 * control: throwing a recording away is not one of the two things this row is
 * open for. Named buttons rather than icons: an icon row is exactly the clutter
 * the collapsed row was cleared of. Outline rather than ghost so each one
 * carries a hairline in both themes instead of reading as loose words under
 * the player. */
export const HistoryRowControls: React.FC<HistoryRowControlsProps> = ({
  menuActions,
  hasText,
  busy,
  showCopied,
  onCopy,
  onRetranscribe,
  onDelete,
}) => {
  const { t } = useTranslation();

  return (
    <div
      className="flex flex-wrap items-center gap-2"
      data-testid="history-entry-controls"
    >
      <Button
        variant="outline"
        size="sm"
        onClick={onCopy}
        disabled={!hasText || busy}
        data-testid="history-entry-copy"
      >
        {showCopied ? (
          <Check aria-hidden="true" className="size-4" />
        ) : (
          <Copy aria-hidden="true" className="size-4" />
        )}
        {showCopied
          ? t("libraryV2.actions.copied")
          : t("libraryV2.actions.copy")}
      </Button>

      <Button
        variant="outline"
        size="sm"
        onClick={onRetranscribe}
        disabled={busy}
        data-testid="history-entry-retry"
      >
        <RotateCcw aria-hidden="true" className="size-4" />
        {t("libraryV2.actions.transcribeAgain")}
      </Button>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            className="text-gray-800 hover:text-gray-1000"
            aria-label={t("common.more")}
            data-testid="history-entry-actions"
          >
            <MoreHorizontal aria-hidden="true" className="size-4" />
          </Button>
        </DropdownMenuTrigger>
        {/* No fixed width. Several translated labels need more than 200px, and
         * sizing the menu to its content avoids clipping in every locale. */}
        <DropdownMenuContent align="end">
          {menuActions.map((action) => (
            <DropdownMenuItem
              key={action.id}
              disabled={action.disabled}
              onSelect={action.onSelect}
              data-testid={`history-entry-${action.id}`}
            >
              {action.label}
            </DropdownMenuItem>
          ))}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            variant="destructive"
            disabled={busy}
            onSelect={onDelete}
            data-testid="history-entry-delete"
          >
            {t("libraryV2.actions.delete")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
};
