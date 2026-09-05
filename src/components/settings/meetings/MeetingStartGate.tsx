import React, { useMemo, useState } from "react";
import { ArrowLeft, RefreshCcw } from "lucide-react";
import { useTranslation } from "react-i18next";
import type {
  MeetingConsentInput,
  MeetingReviewSnapshot,
  SourceKind,
} from "@/bindings";
import {
  PageTitle,
  SETTINGS_SURFACE,
  SettingsPage,
  SettingsSection,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Checkbox } from "@/components/vg/checkbox";
import { useSettingsStore } from "@/stores/settingsStore";
import { MeetingPreviewCard } from "./MeetingPreviewCard";
import { MeetingSourceList } from "./MeetingStatus";
import type { MeetingStartOptions } from "./meetingTypes";
import { preflightAllowsAction } from "./meetingUtils";

/* The only screen left between pressing Start and recording, and it appears
 * exactly when pressing Start could not work: the session exists but a source
 * it was told to record is unavailable.
 *
 * It is not a wizard step. It names the one thing that is wrong and offers the
 * two honest ways out — fix it and retry, or record without that source and
 * carry the partial mark. The wrong source is named once, in the list of what
 * this will record: that list already prints every source's availability in
 * its own tone, so the card that used to repeat the blocked ones above it was
 * the same fact twice on one screen. Round 7 took the two rows that sat under
 * it as well — a "Storage" row whose value read "Encrypted meeting storage is
 * available" and a "Local model" row reading "Available" are the preflight
 * reporting that it succeeded, on the one screen that exists because
 * something failed. Storage now speaks only when it is broken, and a missing
 * local model is Insights' subject, at the moment it stops notes from being
 * written.
 *
 * The assurance sentence sits directly above the action row here too, on the
 * page rather than behind an affordance, because this is one of the three
 * paths that send the consent flags and those flags may only claim what the
 * person could read before pressing. */

interface MeetingStartGateProps {
  snapshot: MeetingReviewSnapshot;
  options: MeetingStartOptions;
  refreshing: boolean;
  starting: boolean;
  onRefresh: () => void;
  onCancel: () => void;
  onStart: (consent: MeetingConsentInput) => void;
}

export const MeetingStartGate: React.FC<MeetingStartGateProps> = ({
  snapshot,
  options,
  refreshing,
  starting,
  onRefresh,
  onCancel,
  onStart,
}) => {
  const { t } = useTranslation();
  const [partialAccepted, setPartialAccepted] = useState(false);
  const notesTemplate = useSettingsStore(
    (state) => state.settings?.meeting_notes_template ?? null,
  );
  const blockedSources = useMemo(
    () =>
      snapshot.session.sources.filter(
        (source) => source.required && source.availability !== "available",
      ),
    [snapshot.session.sources],
  );
  const blocked = blockedSources.length > 0;
  const storageAvailable = snapshot.session.storage === "available";
  const canStart = preflightAllowsAction(snapshot.session, "start");

  /* Consent flags are populated here: the click on the labelled Start button
   * rendered below the assurance line on this screen is the operator's
   * acknowledgment, and the MeetingConsent row records that act. */
  const start = (acceptPartial: boolean) =>
    onStart(
      consentFor(
        options,
        acceptPartial ? blockedSources.map((source) => source.source_kind) : [],
        acceptPartial,
      ),
    );

  const refresh = (
    <Button
      type="button"
      variant="outline"
      onClick={onRefresh}
      disabled={
        refreshing ||
        starting ||
        !preflightAllowsAction(snapshot.session, "refresh_preflight")
      }
    >
      <RefreshCcw aria-hidden="true" />
      {refreshing
        ? t("meetings.preflight.refreshing", "Checking…")
        : t("meetings.actions.refresh")}
    </Button>
  );

  return (
    <SettingsPage
      header={
        <div className="flex flex-col gap-3">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="-ms-2.5 self-start"
            onClick={onCancel}
            disabled={starting}
          >
            <ArrowLeft aria-hidden="true" />
            {t("meetings.actions.back")}
          </Button>
          <PageTitle>
            {blocked || !canStart
              ? t("meetings.gate.title", "Recording did not start")
              : t("meetings.gate.readyTitle", "Ready to record")}
          </PageTitle>
        </div>
      }
    >
      {/* What is about to be recorded, when the operator got here from a
       * meeting Sona had already identified. The card carries no Start of its
       * own: this screen's action row below is the consent act, and a second
       * affirmative button would make it ambiguous which press was recorded
       * as the acknowledgment. Sources read as settled text here because the
       * session already exists with them. */}
      {options.preview === null ? null : (
        <ul className={SETTINGS_SURFACE}>
          <MeetingPreviewCard
            facts={options.preview}
            defaultExpanded
            recording={{ armed: options.sources }}
            notesTemplate={notesTemplate}
          />
        </ul>
      )}

      <SettingsSection label={t("meetings.gate.sources")}>
        <MeetingSourceList
          sources={snapshot.session.sources}
          phase={snapshot.session.phase}
          label={t("meetings.gate.sources")}
        />
      </SettingsSection>

      {storageAvailable ? null : (
        <p role="status" className="text-[14px] leading-[21px] text-red-900">
          {t("meetings.preflight.storageUnavailable")}
        </p>
      )}

      <div className="flex flex-col gap-4">
        <p className="text-[14px] leading-[21px] text-pretty text-gray-1000">
          {t("meetings.start.assurance")}
        </p>

        {blocked ? (
          <div className="flex items-start gap-2.5">
            <Checkbox
              id="gate-accept-partial"
              className="mt-0.5"
              checked={partialAccepted}
              disabled={starting}
              onCheckedChange={() => setPartialAccepted(!partialAccepted)}
            />
            <label
              htmlFor="gate-accept-partial"
              className="text-pretty text-[14px] leading-[21px] text-gray-900"
            >
              {t(
                "meetings.gate.recordAnywayHint",
                "The meeting is marked partial, and the missing source is named in it.",
              )}
            </label>
          </div>
        ) : null}

        {canStart ? null : (
          <p role="status" className="text-[14px] leading-[21px] text-red-900">
            {t("meetings.reasons.invalid_transition")}
          </p>
        )}

        <div className="flex flex-wrap items-center justify-end gap-2">
          {refresh}
          {blocked ? (
            <Button
              type="button"
              onClick={() => start(true)}
              disabled={!partialAccepted || starting || !canStart}
            >
              {starting
                ? t("meetings.start.starting", "Starting…")
                : t("meetings.gate.recordAnyway", "Record without it")}
            </Button>
          ) : (
            <Button
              type="button"
              onClick={() => start(false)}
              disabled={starting || !canStart}
            >
              {starting
                ? t("meetings.start.starting", "Starting…")
                : t("meetings.start.action")}
            </Button>
          )}
        </div>
      </div>
    </SettingsPage>
  );
};

/* The consent policy every acknowledgement on this machine is stamped with.
 *
 * Named here because this module owns what an acknowledgement is. D28's
 * always-record toggle makes the same acknowledgement ahead of time, for a
 * whole series, and cites this same version — two spellings of it would let a
 * standing grant claim a policy the per-attempt receipt never used. */
export const MEETING_CONSENT_POLICY_VERSION = 1;

/* Consent, in the wire shape the backend persists per attempt.
 *
 * The click on the labelled Start button below the assurance line is the
 * operator's acknowledgment; the MeetingConsent row records that act. There is
 * no separate tick box any more, so a caller that sets these flags from a
 * surface without the assurance sentence on screen would make the row assert
 * an acknowledgment nobody could have made. */
export const consentFor = (
  options: MeetingStartOptions,
  acceptedMissingSources: SourceKind[],
  acceptPartial: boolean,
): MeetingConsentInput => ({
  policy_version: MEETING_CONSENT_POLICY_VERSION,
  microphone_acknowledged: options.sources.includes("microphone"),
  system_audio_acknowledged: options.sources.includes("system_audio"),
  known_missing_sources_acknowledged: acceptedMissingSources,
  degraded_start_policy: acceptPartial
    ? "continue_and_mark_partial"
    : options.degradedStartPolicy,
  destination: options.destination,
  remote_acknowledgement: null,
});
