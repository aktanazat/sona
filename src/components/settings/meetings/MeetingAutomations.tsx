import React, { useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type MeetingAutomationKind,
  type MeetingAutomationRoster,
  type MeetingAutomationSeries,
  type MeetingSeriesAutomation,
  type SavedPrompt,
} from "@/bindings";
import {
  Microlabel,
  Notice,
  SettingsDisclosure,
  SettingsRow,
  SettingsSection,
} from "@/components/settings/rows";
import { Input } from "@/components/vg/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { Switch } from "@/components/vg/switch";
import { meetingErrorKey } from "./meetingUtils";

/* D22: what each recorded series does once its notes are written.
 *
 * One disclosure per series, three rows inside it, and nothing on screen until
 * somebody opens one — a person with a dozen recurring meetings should see a
 * dozen quiet lines, not thirty-six switches.
 *
 * Two shapes of write live here and they behave the same way: at the point of
 * intent. A switch writes the moment it is pressed. A text field writes when
 * the person is done with it — Enter, or leaving the field — because a URL is
 * half-typed for most of the time it exists and every keystroke is not a
 * decision. Neither needs a press to confirm the press.
 *
 * Every write carries the shared revision, so a second window editing another
 * series fences this one. A rejection is not an error to apologise for — it is
 * "read again", which is exactly what `refresh` does. */

const KINDS = ["reminders", "shortcut", "webhook", "run_prompt"] as const;

/** The `target` a row currently holds, or the empty string for none. */
const targetOf = (
  series: MeetingAutomationSeries,
  kind: MeetingAutomationKind,
): string => automationOf(series, kind)?.target ?? "";

const automationOf = (
  series: MeetingAutomationSeries,
  kind: MeetingAutomationKind,
): MeetingSeriesAutomation | undefined =>
  series.automations.find((automation) => automation.kind === kind);

const isEnabled = (
  series: MeetingAutomationSeries,
  kind: MeetingAutomationKind,
): boolean => automationOf(series, kind)?.enabled ?? false;

/** Local edits, keyed by series and kind, so one open field never leaks into
 * another row when the roster is re-read under it. */
type Drafts = Record<string, string>;

const draftKey = (seriesKey: string, kind: MeetingAutomationKind) =>
  `${seriesKey}\u0000${kind}`;

export const MeetingAutomations: React.FC = () => {
  const { t } = useTranslation();
  const [roster, setRoster] = useState<MeetingAutomationRoster | null>(null);
  const [drafts, setDrafts] = useState<Drafts>({});
  const [prompts, setPrompts] = useState<SavedPrompt[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [remindersDenied, setRemindersDenied] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      // The prompt list rides along because one kind points at a prompt, and
      // a row offering a picker of ids nobody can read is not a picker.
      const [result, promptList] = await Promise.all([
        commands.meetingAutomationRoster(),
        commands.savedPromptList(),
      ]);
      if (result.status === "error") {
        setError(t(meetingErrorKey(result.error)));
        return;
      }
      setRoster(result.data);
      setPrompts(promptList.status === "ok" ? promptList.data.prompts : []);
      setDrafts({});
      setError(null);
    } catch {
      setError(t("meetings.errors.load"));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const write = async (
    series: MeetingAutomationSeries,
    kind: MeetingAutomationKind,
    enabled: boolean,
    target: string,
  ) => {
    if (!roster) return;
    const key = draftKey(series.series_key, kind);
    setSaving(key);
    setError(null);
    try {
      const result = await commands.meetingSeriesAutomationSet({
        operation_id: crypto.randomUUID(),
        series_key: series.series_key,
        kind,
        enabled,
        target: target.trim() === "" ? null : target.trim(),
        expected_revision: roster.revision,
      });
      if (result.status === "error") {
        setError(
          result.error === "invalid_request"
            ? t("settings.meetings.automations.invalid")
            : t(meetingErrorKey(result.error)),
        );
        return;
      }
      setRemindersDenied(
        kind === "reminders" &&
          enabled &&
          result.data.reminders_access !== "authorized",
      );
      if (result.data.mutation.receipt.result === "rejected") {
        /* Somebody else moved the revision. The refusal changed nothing, so the
         * honest response is to show what is true now rather than to retry
         * behind the operator's back. */
        setError(t("settings.meetings.automations.rejected"));
      }
      await refresh();
    } catch {
      setError(t("settings.meetings.automations.saveFailed"));
    } finally {
      setSaving(null);
    }
  };

  if (loading && roster === null) {
    return (
      <SettingsSection label={t("settings.meetings.automations.title")}>
        <SettingsRow label={t("settings.meetings.automations.loading")} />
      </SettingsSection>
    );
  }

  return (
    <SettingsSection label={t("settings.meetings.automations.title")}>
      {/* Empty is the common state on a Mac that has recorded nothing, and it
       * needs one line, not a sentence about what automations are plus a row
       * with a label and no control. Once a series exists, that sentence is
       * the only statement of when any of this runs. */}
      {roster && roster.series.length === 0 ? (
        <p className="px-6 py-3.5 text-[13px] leading-5 text-gray-800">
          {t("settings.meetings.automations.empty")}
        </p>
      ) : (
        <div className="px-6 py-3">
          <Microlabel>
            {t("settings.meetings.automations.description")}
          </Microlabel>
        </div>
      )}
      {roster?.series.map((series) => (
        <SettingsDisclosure
          key={series.series_key}
          label={series.title || series.series_key}
          fact={t("settings.meetings.automations.seriesFact", {
            count: series.meeting_count,
            when: new Date(series.last_met_at_utc_ms).toLocaleDateString(),
          })}
        >
          {KINDS.map((kind) => (
            <AutomationRow
              key={kind}
              kind={kind}
              series={series}
              prompts={prompts}
              draft={drafts[draftKey(series.series_key, kind)]}
              busy={saving === draftKey(series.series_key, kind)}
              onDraft={(value) =>
                setDrafts((held) => ({
                  ...held,
                  [draftKey(series.series_key, kind)]: value,
                }))
              }
              onWrite={(enabled, target) =>
                void write(series, kind, enabled, target)
              }
            />
          ))}
        </SettingsDisclosure>
      ))}
      {remindersDenied ? (
        <div className="px-6 py-2.5">
          <Notice tone="warning">
            {t("settings.meetings.automations.remindersDenied")}
          </Notice>
        </div>
      ) : null}
      {error ? (
        <div role="alert" className="px-6 py-2.5">
          <Notice tone="danger" live={false}>
            {error}
          </Notice>
        </div>
      ) : null}
    </SettingsSection>
  );
};

const LABEL_KEYS = {
  reminders: "settings.meetings.automations.reminders",
  shortcut: "settings.meetings.automations.shortcut",
  webhook: "settings.meetings.automations.webhook",
  run_prompt: "settings.meetings.automations.runPrompt",
} as const;

const HINT_KEYS = {
  reminders: "settings.meetings.automations.remindersHint",
  shortcut: "settings.meetings.automations.shortcutHint",
  webhook: "settings.meetings.automations.webhookHint",
  run_prompt: "settings.meetings.automations.runPromptHint",
} as const;

const PLACEHOLDER_KEYS = {
  shortcut: "settings.meetings.automations.shortcutPlaceholder",
  webhook: "settings.meetings.automations.webhookPlaceholder",
} as const;

const BLOCKED_KEYS = {
  shortcut: "settings.meetings.automations.needsTarget",
  webhook: "settings.meetings.automations.needsUrl",
  run_prompt: "settings.meetings.automations.needsPrompt",
} as const;

interface AutomationRowProps {
  kind: MeetingAutomationKind;
  series: MeetingAutomationSeries;
  prompts: SavedPrompt[];
  draft: string | undefined;
  busy: boolean;
  onDraft: (value: string) => void;
  onWrite: (enabled: boolean, target: string) => void;
}

/* One kind on one series. The switch is disabled while the field it depends on
 * is empty, which is the honest way to say "this cannot run yet": the backend
 * refuses that write, and offering a switch that is guaranteed to fail would be
 * a control that lies. */
const AutomationRow: React.FC<AutomationRowProps> = ({
  kind,
  series,
  prompts,
  draft,
  busy,
  onDraft,
  onWrite,
}) => {
  const { t } = useTranslation();
  const fieldId = useId();
  const saved = targetOf(series, kind);
  const value = draft ?? saved;
  const enabled = isEnabled(series, kind);
  /* The kind of thing this row addresses, or null for `reminders` — the one
   * kind with nothing to point at. `typedKind` narrows again to the two whose
   * target is a string the operator types; the fourth is a prompt they pick.
   * Carrying the narrowed kinds rather than booleans is what lets the key
   * lookups below stay lookups. */
  const targetKind = kind === "reminders" ? null : kind;
  const typedKind = targetKind === "run_prompt" ? null : targetKind;
  const dirty = typedKind !== null && value.trim() !== saved.trim();
  const blocked = targetKind !== null && value.trim() === "";

  /* Enter and blur are the same act: Enter blurs the field, and leaving it is
   * the one thing that writes. A draft equal to what is stored is not a
   * change, and an empty target is refused by the backend, so neither reaches
   * the command. */
  const commit = () => {
    if (!dirty || blocked || busy) return;
    onWrite(enabled, value);
  };

  return (
    <SettingsRow
      label={t(LABEL_KEYS[kind])}
      hint={t(HINT_KEYS[kind])}
      controlId={targetKind !== null ? fieldId : undefined}
      fact={
        targetKind !== null && blocked && !enabled
          ? t(BLOCKED_KEYS[targetKind])
          : undefined
      }
    >
      {typedKind !== null ? (
        <Input
          id={fieldId}
          className="w-56"
          value={value}
          disabled={busy}
          placeholder={t(PLACEHOLDER_KEYS[typedKind])}
          onChange={(changed) => onDraft(changed.target.value)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
        />
      ) : null}
      {/* A pick is a decision, so it writes on the spot — unlike a URL, which
          is half-typed for most of the time it exists. */}
      {kind === "run_prompt" ? (
        <Select
          value={value === "" ? undefined : value}
          disabled={busy || prompts.length === 0}
          onValueChange={(next) => onWrite(enabled, next)}
        >
          <SelectTrigger id={fieldId} size="sm" className="w-56">
            <SelectValue
              placeholder={t("settings.meetings.automations.promptPlaceholder")}
            />
          </SelectTrigger>
          <SelectContent>
            {prompts.map((prompt) => (
              <SelectItem key={prompt.prompt_id} value={prompt.prompt_id}>
                {prompt.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      ) : null}
      <Switch
        checked={enabled}
        disabled={busy || (blocked && !enabled)}
        aria-label={t(LABEL_KEYS[kind])}
        onCheckedChange={(next) => onWrite(next, value)}
      />
    </SettingsRow>
  );
};
