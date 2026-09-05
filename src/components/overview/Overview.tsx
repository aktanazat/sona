import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { MoreHorizontal } from "lucide-react";
import { commands, events, type HistoryTrendProjection } from "@/bindings";
import { useAudioImport } from "@/hooks/useAudioImport";
import { useSettings } from "@/hooks/useSettings";
import { useOsType } from "@/hooks/useOsType";
import { formatKeyCombination, keyCapParts } from "@/lib/utils/keyboard";
import { cn } from "@/lib/cn";
import {
  PAGE_COLUMN,
  SETTINGS_SURFACE,
  SettingsCard,
} from "@/components/settings/rows";
import { Aurora } from "@/components/Aurora";
import { Button } from "@/components/vg/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import { Kbd } from "@/components/vg/kbd";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/vg/tooltip";
import { checkForUpdates, type UpdateCheckResult } from "@/lib/updateCheck";
import { waitForFirstVisibleFrame } from "@/lib/launchTrace";
import { ActivityBand } from "./ActivityBand";
import { CaptureModeChip } from "./CaptureModeChip";
import {
  NeedsYou,
  Recent,
  useLearningRows,
  useOverviewFeed,
} from "./OverviewFeed";

/* Capture shows one thing: what dictation is doing, and what needs the reader.
 * This week's numbers and what Sona did on its own are two closed rows under
 * it, each carrying the measurement that decides whether to open it. */

/* Recording is a command, not an event: the backend starts and stops on a
 * global chord this window never sees, so the status word is polled. One
 * boolean a second is the whole backend cost of this page while it is open. */
const RECORDING_POLL_MS = 1000;

const subscribeToActivityUpdates = (reload: () => void): (() => void) => {
  const subscription = events.historyUpdatePayload.listen((event) => {
    if (event.payload.action !== "toggled") reload();
  });
  return () => {
    void subscription.then((unlisten) => unlisten());
  };
};

export interface CaptureHeroProps {
  isRecording: boolean;
  /** The raw chord, or null when nothing is bound. */
  binding: string | null;
  pushToTalk: boolean;
  /** An import dialog is already open. */
  importing: boolean;
  onNewMeeting: () => void;
  onImportAudio: () => void;
  onRecordScreen: () => void;
  onChangeShortcut: () => void;
  /** Opens the Modes editor, from the mode chip's one footer line. */
  onOpenModes: () => void;
}

/**
 * The page's one surface. Everything it draws is passed in, because the state
 * behind it is polled, dialog-driven or read from the settings store — none of
 * which is what this card is: the state word, the chord drawn once, and its
 * one direct action.
 */
export const CaptureHero: React.FC<CaptureHeroProps> = ({
  isRecording,
  binding,
  pushToTalk,
  importing,
  onNewMeeting,
  onImportAudio,
  onRecordScreen,
  onChangeShortcut,
  onOpenModes,
}) => {
  const { t } = useTranslation();
  const osType = useOsType();
  const keys =
    binding === null
      ? []
      : keyCapParts(binding, osType).filter((key) => key.length > 0);
  /* One click starts a meeting, so the promise sits with the button rather than
   * behind a wizard step nobody reads. The key lives in the meetings subtree,
   * which owns this sentence's exact wording in every locale. */
  const assurance = t("meetings.start.assurance");

  return (
    <SettingsCard
      aria-labelledby="overview-status"
      className="relative overflow-hidden px-6 py-5"
    >
      <Aurora isRecording={isRecording} />
      <div className="relative flex flex-col gap-5">
        <div className="flex flex-col gap-2">
          <h1
            id="overview-status"
            aria-live="polite"
            data-recording={isRecording ? "true" : undefined}
            /* The document-title size from the round-6 type scale, in explicit
             * px: this app sets `:root { font-size: 14px }` (styles/base.css),
             * so every rem utility renders at 87.5% of its name. One word, at
             * the same size a meeting's title is set in, so the page does not
             * shout a state that every other page states quietly. */
            className={cn(
              "text-[24px] leading-[30px] font-semibold tracking-[-0.01em] text-balance",
              isRecording ? "text-accent-strong" : "text-gray-1000",
            )}
          >
            {t(isRecording ? "overview.hero.recording" : "overview.hero.ready")}
          </h1>

          {/* One meta line under the state word: the chord that starts a
           * dictation, the gesture it answers to, and the mode the next one
           * runs in. Three sentence fragments stacked as three paragraphs read
           * as three unrelated announcements; they are one sentence about the
           * next dictation. Nothing bound means no keycaps and no gesture —
           * printing either would claim a capability this install lacks. */}
          <div className="flex min-h-5 flex-wrap items-center gap-x-2 gap-y-1 text-[13px] leading-[18px] text-gray-900">
            {keys.length === 0 ? (
              /* Bordered, not ghost: a ghost button at rest has no border and
               * no fill, so this read as a sentence fragment where it is the
               * one control that fixes an install with no chord bound. */
              <Button
                type="button"
                variant="outline"
                size="xs"
                onClick={onChangeShortcut}
                data-testid="overview-shortcut"
              >
                {t("overview.hero.setShortcutAction")}
              </Button>
            ) : (
              <>
                <button
                  type="button"
                  onClick={onChangeShortcut}
                  /* The left/right qualifier the caps drop, one hover away. */
                  title={formatKeyCombination(binding ?? "", osType)}
                  aria-label={t("overview.hero.shortcutAction")}
                  data-testid="overview-shortcut"
                  className="hover-fast -mx-1 inline-flex items-center gap-1 rounded-md px-1 py-0.5 hover:bg-gray-alpha-100 focus-visible:ring-2 focus-visible:ring-focus-ring focus-visible:outline-none"
                >
                  {keys.map((key, index) => (
                    <Kbd key={`${key}-${index}`}>{key}</Kbd>
                  ))}
                </button>
                <span>
                  {t(
                    pushToTalk
                      ? "overview.hero.gestureTapHold"
                      : "overview.hero.gestureTapOnly",
                  )}
                </span>
              </>
            )}
            <span aria-hidden="true" className="text-gray-700">
              ·
            </span>
            {/* Modes left the rail, so this is where one gets picked: the name
             * of the mode the next dictation runs in, one click from the list. */}
            <span>{t("modesV2.chip.lead")}</span>
            <CaptureModeChip onOpenModes={onOpenModes} />
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-1">
          {/* The promise lives in the tooltip and nowhere else. Radix opens the
           * tooltip on focus and points the trigger's aria-describedby at the
           * content while it is open, so a keyboard or screen-reader user reaches
           * this sentence by tabbing to the button — the primitive already does
           * the job a second permanent copy of the sentence was doing here, and
           * that copy also displaced Radix's own wiring (a child's
           * aria-describedby wins the Slot merge). One datum, one place. */}
          <Tooltip>
            <TooltipTrigger asChild>
              <Button type="button" size="sm" onClick={onNewMeeting}>
                {t("overview.hero.newMeeting")}
              </Button>
            </TooltipTrigger>
            <TooltipContent>{assurance}</TooltipContent>
          </Tooltip>
          {/* The other two ways to get audio in. They are not what this page is
           * for, and a row of three equal buttons said they were. */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("common.more")}
                data-testid="overview-more"
              >
                <MoreHorizontal aria-hidden="true" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start">
              {osType === "macos" ? (
                <DropdownMenuItem onSelect={onRecordScreen}>
                  {t("recorder.open")}
                </DropdownMenuItem>
              ) : null}
              <DropdownMenuItem disabled={importing} onSelect={onImportAudio}>
                {t("overview.hero.importAudio")}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
    </SettingsCard>
  );
};

interface OverviewProps {
  /** The shell's section setter for Capture's direct actions. */
  onOpenSection?: (section: "meetings" | "settings" | "modes") => void;
  /** Opens the retained meeting named by a workflow receipt or commitment. */
  onOpenMeeting?: (meetingId: string) => void;
  /** Opens the native screen recorder without creating a second destination. */
  onOpenRecorder: () => void;
}

export const Overview: React.FC<OverviewProps> = ({
  onOpenSection,
  onOpenMeeting,
  onOpenRecorder,
}) => {
  const { settings } = useSettings();
  const [isRecording, setIsRecording] = useState(false);
  const [activityTrend, setActivityTrend] =
    useState<HistoryTrendProjection | null>(null);
  /* No options: Capture has nowhere of its own to draw a failure, so it takes
   * the shared action's toast. It used to swallow the error entirely and tell
   * the reader to go look in Library, which is not where they were. */
  const { start: startAudioImport, importing } = useAudioImport();
  const { receipts, openLoops, refresh } = useOverviewFeed();
  const learning = useLearningRows();
  const [updateResult, setUpdateResult] = useState<UpdateCheckResult | null>(
    null,
  );
  /* Separate from the result because a check that came back with nothing and a
   * check still out are different answers, and only one of them lets the
   * section below say nothing needs the reader. */
  const [updateChecked, setUpdateChecked] = useState(false);
  const [updateDismissed, setUpdateDismissed] = useState(false);

  useEffect(() => {
    let active = true;
    let interval: number | undefined;
    const refreshRecording = async () => {
      try {
        const recording = await commands.isRecording();
        if (active) setIsRecording(recording);
      } catch {
        if (active) setIsRecording(false);
      }
    };
    const syncPolling = () => {
      if (document.hidden) {
        if (interval !== undefined) {
          window.clearInterval(interval);
          interval = undefined;
        }
        return;
      }
      void refreshRecording();
      interval ??= window.setInterval(
        () => void refreshRecording(),
        RECORDING_POLL_MS,
      );
    };
    syncPolling();
    document.addEventListener("visibilitychange", syncPolling);
    return () => {
      active = false;
      document.removeEventListener("visibilitychange", syncPolling);
      if (interval !== undefined) window.clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    let active = true;

    const refreshTrend = async () => {
      try {
        const result = await commands.getHistoryTrend({ range: "days_180" });
        if (active) {
          setActivityTrend(result.status === "ok" ? result.data : null);
        }
      } catch {
        if (active) setActivityTrend(null);
      }
    };

    void refreshTrend();
    const stopListening = subscribeToActivityUpdates(() => void refreshTrend());

    return () => {
      active = false;
      stopListening();
    };
  }, []);

  /* One check per visit, after the launch shell has composited. The backend
   * still owns the preference decision; disabled checks make no request. A
   * check that does not come back says nothing about the app in front of you,
   * so nothing is drawn for it here — Settings > About owns that sentence and
   * the button that asks again. */
  const runUpdateCheck = useCallback(async () => {
    try {
      setUpdateResult(await checkForUpdates());
    } catch {
      setUpdateResult(null);
    } finally {
      setUpdateChecked(true);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void waitForFirstVisibleFrame().then(() => {
      if (!cancelled) void runUpdateCheck();
    });
    return () => {
      cancelled = true;
    };
  }, [runUpdateCheck]);

  return (
    /* State, then what needs you, then the two closed rows. The page is read in
     * that order, from the same origin every other page starts at: centering it
     * would slide the whole column up the moment a disclosure grew past the
     * window, moving the row under the pointer that opened it. */
    <div className={cn(PAGE_COLUMN, "flex flex-col gap-8 pt-8 pb-[72px]")}>
      <CaptureHero
        isRecording={isRecording}
        binding={
          settings?.bindings?.transcribe?.current_binding?.trim() || null
        }
        pushToTalk={settings?.push_to_talk ?? true}
        importing={importing}
        onNewMeeting={() => onOpenSection?.("meetings")}
        onImportAudio={() => void startAudioImport()}
        onRecordScreen={onOpenRecorder}
        onChangeShortcut={() => onOpenSection?.("settings")}
        onOpenModes={() => onOpenSection?.("modes")}
      />

      <NeedsYou
        openLoops={openLoops}
        learning={learning.entries}
        update={updateDismissed ? null : updateResult}
        updateChecked={updateChecked}
        onAnswerLearning={learning.answer}
        onDismissUpdate={() => setUpdateDismissed(true)}
        onOpenMeeting={(meetingId) => onOpenMeeting?.(meetingId)}
        onRetry={refresh}
      />

      <div className={SETTINGS_SURFACE}>
        {activityTrend === null ? null : <ActivityBand trend={activityTrend} />}
        <Recent
          receipts={receipts}
          onOpenMeeting={(meetingId) => onOpenMeeting?.(meetingId)}
          onRetry={refresh}
        />
      </div>
    </div>
  );
};
