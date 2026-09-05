import React from "react";
import { SettingsPage } from "../rows";
import { HistoryAudioImportSection } from "./HistoryAudioImportSection";
import { HistoryImportLive } from "./HistoryImportLive";
import { HistoryFeed } from "./HistoryFeed";
import { HistoryTitleBar } from "./HistoryTitleBar";
import { HistorySummary } from "./HistorySummary";
import { useHistoryData } from "./useHistoryData";

/* The `sona://dictation/<id>` address the shell was asked to open. The shell
 * holds one of these and this page consumes it, so the shape has one name. */
export interface DictationRequest {
  historyId: number;
  nonce: number;
}

export const HistorySettings: React.FC<{
  dictationRequest?: DictationRequest | null;
}> = ({ dictationRequest = null }) => {
  const {
    state,
    query,
    setQuery,
    view,
    setView,
    activeQuery,
    receiptsByHistoryId,
    audioImportJobs,
    audioImportError,
    startingAudioImport,
    historyStats,
    statsLoading,
    statsError,
    refreshHistoryStats,
    fetchPage,
    toggleSaved,
    copyToClipboard,
    getAudioBlob,
    deleteEntry,
    retryHistoryEntry,
    sentinelRef,
    startAudioImport,
    cancelAudioImport,
    openRecordingsFolder,
  } = useHistoryData();

  return (
    /* The column and the type still come from the shared primitive, so Library
     * cannot drift from every other settings page. The whole of the page's
     * chrome is the primitive's `header` slot: one title line, then one Meta
     * line under it — the totals, or the match count while a search is
     * running, because the page has one question open at a time. */
    <SettingsPage
      header={
        <div className="flex min-w-0 flex-col gap-2">
          <HistoryTitleBar
            state={state}
            query={query}
            setQuery={setQuery}
            view={view}
            setView={setView}
            activeQuery={activeQuery}
            importing={startingAudioImport}
            onImport={() => void startAudioImport()}
            onOpenFolder={() => void openRecordingsFolder()}
          />
          {activeQuery.trim() === "" ? (
            <HistorySummary
              stats={historyStats}
              loading={statsLoading}
              error={statsError}
              onRetry={() => void refreshHistoryStats()}
            />
          ) : null}
          {/* Always mounted, and empty it takes no space: a live region that
           * appears with its first message loses that message. */}
          <HistoryImportLive jobs={audioImportJobs} />
        </div>
      }
    >
      <HistoryAudioImportSection
        jobs={audioImportJobs}
        error={audioImportError}
        onCancel={cancelAudioImport}
      />

      <HistoryFeed
        state={state}
        setQuery={setQuery}
        view={view}
        activeQuery={activeQuery}
        focusRequest={dictationRequest}
        sentinelRef={sentinelRef}
        receiptsByHistoryId={receiptsByHistoryId}
        toggleSaved={toggleSaved}
        copyToClipboard={copyToClipboard}
        getAudioBlob={getAudioBlob}
        deleteEntry={deleteEntry}
        retryHistoryEntry={retryHistoryEntry}
        fetchPage={fetchPage}
      />
    </SettingsPage>
  );
};
