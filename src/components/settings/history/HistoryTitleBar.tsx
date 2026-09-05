import React, { useId } from "react";
import { MoreHorizontal, Search, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { PageTitle } from "../rows";
import { Button } from "@/components/vg/button";
import { Input } from "@/components/vg/input";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import type { HistoryTextView } from "./HistoryEntry";
import type { ListState } from "./historyListReducer";

interface HistoryTitleBarProps {
  state: ListState;
  query: string;
  setQuery: (query: string) => void;
  view: HistoryTextView;
  setView: (view: HistoryTextView) => void;
  activeQuery: string;
  importing: boolean;
  onImport: () => void;
  onOpenFolder: () => void;
}

/**
 * Library's one line: the name of the page, the field that searches it, the
 * one verb that adds to it, and a menu for the rest.
 *
 * The toolbar this replaces sat under the title with three more controls in
 * it — a two-segment Processed/Raw switch, a named folder button, and the
 * match count — so the top of the page held six things and two of them are
 * touched about once a month. Both of those are menu items now: the
 * transcript view is a checkable item, because it is a state and not a
 * command, and the folder is a verb.
 *
 * The match count stays visible, under the row rather than in it: it is the
 * answer to what was just typed, and while a search is running it is the line
 * the page's own totals give way to.
 */
export const HistoryTitleBar: React.FC<HistoryTitleBarProps> = ({
  state,
  query,
  setQuery,
  view,
  setView,
  activeQuery,
  importing,
  onImport,
  onOpenFolder,
}) => {
  const { t } = useTranslation();
  const countId = useId();
  const searching = activeQuery.trim() !== "";
  const settled = state.phase !== "loading" && state.phase !== "error";
  const count = state.entries.length;
  const moreLabel = t("common.more");

  // Only the search result count is announced. A running total that changes
  // on every scroll tick would turn the live region into noise.
  let resultCount = "";
  if (searching && settled) {
    if (count === 0) {
      resultCount = t("settings.history.resultsNone", "No matches");
    } else if (state.hasMore) {
      resultCount = t("settings.history.resultsMore", "{{count}}+ matches", {
        count,
      });
    } else {
      resultCount = t("settings.history.results", "{{count}} matches", {
        count,
      });
    }
  }

  return (
    <div className="flex min-w-0 flex-col gap-1">
      {/* One honest wrap row: the title takes the slack, everything after it
       * is flex-none in DOM order — field, Import, menu — so at width they sit
       * on one line and under it they wrap whole, last first. */}
      <div
        className="flex flex-wrap items-center gap-3"
        data-testid="history-title-line"
      >
        {/* The rail names this destination Library, so the page answers to the
         * same word — one destination, one name. `settings.history.*` keys
         * keep their address; only the visible values moved to the rail's
         * term. */}
        <PageTitle className="min-w-0 flex-1 truncate">
          {t("topNav.library")}
        </PageTitle>

        <div className="relative w-[220px] min-w-0 flex-none">
          <Search
            aria-hidden="true"
            className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-gray-800"
          />
          <Input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("settings.history.searchPlaceholder")}
            aria-label={t("settings.history.search")}
            aria-describedby={countId}
            data-testid="history-search"
            className="h-8 pl-8"
          />
          {query === "" ? null : (
            <Button
              variant="ghost"
              size="icon"
              className="absolute top-1/2 right-1 size-6 -translate-y-1/2 text-gray-800 hover:text-gray-1000"
              aria-label={t("settings.history.clearSearch", "Clear search")}
              onClick={() => setQuery("")}
              data-testid="history-search-clear"
            >
              <X aria-hidden="true" className="size-4" />
            </Button>
          )}
        </div>

        <Button
          size="sm"
          className="flex-none"
          onClick={onImport}
          disabled={importing}
          data-testid="history-import"
        >
          {t("settings.history.import")}
        </Button>

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              className="flex-none text-gray-700 hover:text-gray-1000"
              aria-label={moreLabel}
              title={moreLabel}
              data-testid="history-more"
            >
              <MoreHorizontal aria-hidden="true" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-52">
            {/* A state, so a checkable item and not a command: both views
             * render the same rows, one field of them, and the reader is
             * turning one of the two on rather than going somewhere. */}
            <DropdownMenuCheckboxItem
              checked={view === "raw"}
              onCheckedChange={(checked) =>
                setView(checked ? "raw" : "processed")
              }
              data-testid="history-show-raw"
            >
              {t("settings.history.showRawText")}
            </DropdownMenuCheckboxItem>
            <DropdownMenuItem
              onSelect={onOpenFolder}
              data-testid="history-open-folder"
            >
              {t("settings.history.openFolder")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <p
        id={countId}
        className="text-[13px] leading-[18px] text-gray-900 tabular-nums"
        aria-live="polite"
        data-testid="history-result-count"
      >
        {resultCount}
      </p>
    </div>
  );
};
