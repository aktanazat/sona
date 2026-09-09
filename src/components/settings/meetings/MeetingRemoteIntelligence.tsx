import React, { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type MeetingLocalEngine,
  type MeetingLocalEngineStatus,
  type MeetingSeriesRemoteRow,
} from "@/bindings";
import {
  Notice,
  SettingsField,
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
import { useSettings } from "@/hooks/useSettings";
/* D14: where a meeting's summaries, ledgers, recaps and answers get written.
 *
 * The engine picker chooses Apple Intelligence or the operator's loopback
 * OpenAI-compatible endpoint. The switch routes meeting intelligence to the
 * operator's own server; the sentence says exactly what leaves this Mac when
 * it is on, and it stays on the surface rather than behind an info affordance,
 * because a consent sentence nobody reads is not consent. The list underneath
 * is the escape hatch: a series named here is written on this Mac even while
 * the switch is on.
 *
 * The switch is disabled until a relay is paired. Turning on a route to a
 * server that does not exist would be a setting that claims something untrue,
 * and the backend's own selection reads the same four settings fields this row
 * does, so the two cannot disagree about whether remote work is possible. */
type LocalEndpointEngine = Extract<
  MeetingLocalEngine,
  { kind: "local_endpoint" }
>;

type LocalEndpointDraftFields = {
  baseUrl: string;
  model: string;
  contextWindow: string | number | null;
};

type LocalEngineSelection = LocalEndpointDraftFields & {
  engineKind: "apple_intelligence" | "local_endpoint";
  draft: boolean;
};

const selectMeetingLocalEngine = (
  value: LocalEngineSelection["engineKind"],
  persistedEngine: MeetingLocalEngine | undefined,
  defaultLocalEngine: LocalEndpointEngine | undefined,
  persist: (engine: MeetingLocalEngine) => void,
): LocalEngineSelection => {
  if (value === "apple_intelligence") {
    persist({ kind: "apple_intelligence" });
    return {
      engineKind: value,
      draft: false,
      baseUrl: "",
      model: "",
      contextWindow: "",
    };
  }

  const localEngine =
    persistedEngine?.kind === "local_endpoint"
      ? persistedEngine
      : defaultLocalEngine;
  if (localEngine) {
    persist(localEngine);
  }
  return {
    engineKind: value,
    draft: true,
    baseUrl: localEngine?.base_url ?? "",
    model: localEngine?.model ?? "",
    contextWindow: localEngine?.context_window_tokens?.toString() ?? "",
  };
};

const commitMeetingLocalEndpoint = (
  fields: LocalEndpointDraftFields,
  persistedEngine: LocalEndpointEngine | undefined,
  persist: (engine: LocalEndpointEngine) => void,
): boolean => {
  const baseUrl = fields.baseUrl.trim();
  if (!baseUrl) return false;
  const model = fields.model.trim();
  const rawContextWindow =
    fields.contextWindow === null || String(fields.contextWindow).trim() === ""
      ? null
      : Number(fields.contextWindow);
  if (
    rawContextWindow !== null &&
    (!Number.isSafeInteger(rawContextWindow) || rawContextWindow < 1)
  ) {
    return false;
  }

  const next: LocalEndpointEngine = {
    kind: "local_endpoint",
    base_url: baseUrl,
    model,
    context_window_tokens: rawContextWindow,
  };
  if (
    persistedEngine &&
    next.base_url === persistedEngine.base_url &&
    next.model === persistedEngine.model &&
    next.context_window_tokens ===
      (persistedEngine.context_window_tokens ?? null)
  ) {
    return false;
  }
  persist(next);
  return true;
};

const meetingLocalEngineStatusKey = (error: string | null): string => {
  switch (error) {
    case "context_window_not_configured":
      return "settings.meetings.localEngine.status.endpointContextUnknown";
    case "invalid_endpoint":
      return "settings.meetings.localEngine.status.endpointInvalid";
    case "invalid_response":
      return "settings.meetings.localEngine.status.endpointInvalidResponse";
    case "unreachable":
      return "settings.meetings.localEngine.status.endpointUnreachable";
    default:
      return "settings.meetings.localEngine.status.endpointUnknown";
  }
};

const MeetingLocalEngineSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, getDefaultSetting, updateSetting, isUpdating } =
    useSettings();
  const engineId = useId();
  const baseUrlId = useId();
  const modelId = useId();
  const statusRequest = useRef(0);
  const contextWindowId = useId();
  const defaultEngine = getDefaultSetting("meeting_local_engine");
  const defaultLocalEngine =
    defaultEngine?.kind === "local_endpoint" ? defaultEngine : undefined;
  const persistedEngine = getSetting("meeting_local_engine") ?? defaultEngine;
  const persistedLocalEngine =
    persistedEngine?.kind === "local_endpoint" ? persistedEngine : undefined;
  const saving = isUpdating("meeting_local_engine");
  const [status, setStatus] = useState<MeetingLocalEngineStatus | null>(null);
  const [localDraft, setLocalDraft] = useState(false);
  const engineKind =
    localDraft || persistedLocalEngine
      ? "local_endpoint"
      : (persistedEngine?.kind ?? "");
  const hasLocalDraft = localDraft;
  const [baseUrl, setBaseUrl] = useState(persistedLocalEngine?.base_url ?? "");
  const [model, setModel] = useState(persistedLocalEngine?.model ?? "");
  const [contextWindow, setContextWindow] = useState(
    persistedLocalEngine?.context_window_tokens?.toString() ?? "",
  );

  useEffect(() => {
    if (saving || persistedLocalEngine === undefined) return;
    const draftContextWindow =
      contextWindow.trim() === "" ? null : Number(contextWindow);
    if (
      localDraft &&
      (baseUrl.trim() !== persistedLocalEngine.base_url ||
        model.trim() !== persistedLocalEngine.model ||
        draftContextWindow !==
          (persistedLocalEngine.context_window_tokens ?? null))
    ) {
      return;
    }
    setLocalDraft(false);
    setBaseUrl(persistedLocalEngine.base_url);
    setModel(persistedLocalEngine.model);
    setContextWindow(
      persistedLocalEngine.context_window_tokens?.toString() ?? "",
    );
  }, [
    baseUrl,
    contextWindow,
    localDraft,
    model,
    persistedLocalEngine?.base_url,
    persistedLocalEngine?.model,
    persistedLocalEngine?.context_window_tokens,
    saving,
  ]);
  useEffect(() => {
    const request = ++statusRequest.current;
    setStatus(null);
    if (hasLocalDraft || saving) return;

    void (async () => {
      try {
        const next = await commands.meetingLocalEngineStatus();
        if (request === statusRequest.current) setStatus(next);
      } catch {
        if (request === statusRequest.current) setStatus(null);
      }
    })();
  }, [
    persistedEngine?.kind,
    persistedLocalEngine?.base_url,
    persistedLocalEngine?.model,
    persistedLocalEngine?.context_window_tokens,
    hasLocalDraft,
    saving,
  ]);

  const updateLocalEngine = (overrides: Partial<LocalEndpointEngine>) => {
    if (engineKind !== "local_endpoint") return;
    const contextWindowValue =
      "context_window_tokens" in overrides
        ? (overrides.context_window_tokens ?? null)
        : contextWindow;
    commitMeetingLocalEndpoint(
      {
        baseUrl: overrides.base_url ?? baseUrl,
        model: overrides.model ?? model,
        contextWindow: contextWindowValue,
      },
      persistedLocalEngine,
      (next) => {
        setLocalDraft(true);
        void updateSetting("meeting_local_engine", next);
      },
    );
  };

  const selectEngine = (value: string) => {
    if (
      (value !== "apple_intelligence" && value !== "local_endpoint") ||
      value === engineKind ||
      saving
    ) {
      return;
    }
    const selection = selectMeetingLocalEngine(
      value,
      persistedEngine,
      defaultLocalEngine,
      (next) => void updateSetting("meeting_local_engine", next),
    );
    setLocalDraft(selection.draft);
    setBaseUrl(selection.baseUrl);
    setModel(selection.model);
    setContextWindow(selection.contextWindow?.toString() ?? "");
  };

  const commitBaseUrl = () => {
    const next = baseUrl.trim();
    if (!next) return;
    updateLocalEngine({ base_url: next });
  };

  const commitModel = () => {
    updateLocalEngine({ model: model.trim() });
  };

  const commitContextWindow = () => {
    const next = contextWindow.trim();
    updateLocalEngine({
      context_window_tokens: next === "" ? null : Number(next),
    });
  };

  const endpointStatusText = (error: string | null) =>
    t(meetingLocalEngineStatusKey(error));

  const statusTone = hasLocalDraft
    ? "warning"
    : status?.kind === "apple_intelligence"
      ? status.available
        ? "muted"
        : "warning"
      : status?.kind === "local_endpoint"
        ? status.reachable && status.error === null
          ? "muted"
          : "warning"
        : "muted";
  const statusText = hasLocalDraft
    ? t("settings.meetings.localEngine.status.endpointNotConfigured")
    : status === null
      ? t("settings.meetings.localEngine.status.checking")
      : status.kind === "apple_intelligence"
        ? status.available
          ? t("settings.meetings.localEngine.status.appleAvailable")
          : t("settings.meetings.localEngine.status.appleUnavailable")
        : status.error
          ? endpointStatusText(status.error)
          : t("settings.meetings.localEngine.status.endpointReachable", {
              count: status.model_count,
            });

  return (
    <>
      <SettingsRow
        label={t("settings.meetings.localEngine.label")}
        hint={t("settings.meetings.localEngine.description")}
        controlId={engineId}
        disabled={saving || (persistedEngine === undefined && !hasLocalDraft)}
      >
        <Select
          value={engineKind}
          onValueChange={selectEngine}
          disabled={saving || (persistedEngine === undefined && !hasLocalDraft)}
        >
          <SelectTrigger id={engineId} size="sm" className="w-full">
            <SelectValue>
              {engineKind === "apple_intelligence"
                ? t("settings.meetings.localEngine.apple")
                : engineKind === "local_endpoint"
                  ? t("settings.meetings.localEngine.endpoint")
                  : t("settings.meetings.localEngine.status.checking")}
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="apple_intelligence">
              {t("settings.meetings.localEngine.apple")}
            </SelectItem>
            <SelectItem value="local_endpoint">
              {t("settings.meetings.localEngine.endpoint")}
            </SelectItem>
          </SelectContent>
        </Select>
      </SettingsRow>
      {engineKind === "local_endpoint" ? (
        <>
          <SettingsField
            label={t("settings.meetings.localEngine.baseUrl.label")}
            hint={t("settings.meetings.localEngine.baseUrl.hint")}
            controlId={baseUrlId}
          >
            <Input
              id={baseUrlId}
              value={baseUrl}
              onChange={(event) => {
                setLocalDraft(true);
                setBaseUrl(event.target.value);
              }}
              onBlur={commitBaseUrl}
              disabled={saving}
              placeholder={t(
                "settings.meetings.localEngine.baseUrl.placeholder",
              )}
            />
          </SettingsField>
          <SettingsField
            label={t("settings.meetings.localEngine.model.label")}
            hint={t("settings.meetings.localEngine.model.hint")}
            controlId={modelId}
          >
            <Input
              id={modelId}
              value={model}
              onChange={(event) => {
                setLocalDraft(true);
                setModel(event.target.value);
              }}
              onBlur={commitModel}
              disabled={saving}
              placeholder={t("settings.meetings.localEngine.model.placeholder")}
            />
          </SettingsField>
          <SettingsField
            label={t("settings.meetings.localEngine.contextWindow.label")}
            hint={t("settings.meetings.localEngine.contextWindow.hint")}
            controlId={contextWindowId}
          >
            <Input
              id={contextWindowId}
              type="number"
              min={1}
              step={1}
              value={contextWindow}
              onChange={(event) => {
                setLocalDraft(true);
                setContextWindow(event.target.value);
              }}
              onBlur={commitContextWindow}
              disabled={saving}
              placeholder={t(
                "settings.meetings.localEngine.contextWindow.placeholder",
              )}
            />
          </SettingsField>
        </>
      ) : null}
      <div className="px-6 py-3">
        <Notice tone={statusTone}>{statusText}</Notice>
      </div>
    </>
  );
};
const SeriesRow: React.FC<{
  row: MeetingSeriesRemoteRow;
  locale: string;
  saving: boolean;
  onToggle: (row: MeetingSeriesRemoteRow, optOut: boolean) => void;
}> = ({ row, locale, saving, onToggle }) => {
  const { t } = useTranslation();
  const id = useId();

  return (
    <SettingsRow
      label={row.title || row.series_key}
      fact={t("settings.meetings.remoteIntelligence.lastMet", {
        date: new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(
          new Date(row.last_met_at_utc_ms),
        ),
      })}
      controlId={id}
    >
      <Switch
        id={id}
        aria-label={t("settings.meetings.remoteIntelligence.keepLocalLabel", {
          series: row.title || row.series_key,
        })}
        checked={row.remote_intelligence_opt_out}
        disabled={saving}
        onCheckedChange={(optOut) => onToggle(row, optOut)}
      />
    </SettingsRow>
  );
};

export const MeetingRemoteIntelligence: React.FC = () => {
  const { t, i18n } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const switchId = useId();
  const [rows, setRows] = useState<MeetingSeriesRemoteRow[] | null>(null);
  const [revision, setRevision] = useState(0);
  const [saving, setSaving] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  const enabled = getSetting("meeting_remote_intelligence_enabled") ?? false;
  /* The same four fields the backend's own readiness check reads. A relay is
   * reachable only when the panel is on, a pairing was saved, and the pinned
   * key and its URL are both stored. */
  const paired =
    (getSetting("agent_panel_enabled") ?? false) &&
    (getSetting("agent_panel_paired") ?? false) &&
    getSetting("agent_panel_relay_url") != null &&
    getSetting("agent_panel_relay_key_id") != null &&
    getSetting("agent_panel_relay_public_key") != null;

  const load = useCallback(async () => {
    try {
      const result = await commands.meetingSeriesRemoteRoster();
      if (result.status === "ok") {
        setRows(result.data.rows);
        setRevision(result.data.revision);
        return;
      }
    } catch {
      /* A roster that cannot be read costs the list, not the switch. */
    }
    setRows([]);
  }, []);

  useEffect(() => {
    if (!enabled) {
      setRows(null);
      return;
    }
    void load();
  }, [enabled, load]);

  const toggleSeries = (row: MeetingSeriesRemoteRow, optOut: boolean) => {
    setSaving(row.series_key);
    setFailed(false);
    void (async () => {
      try {
        const result = await commands.meetingSeriesRemoteOptOutSet({
          operation_id: crypto.randomUUID(),
          series_key: row.series_key,
          remote_intelligence_opt_out: optOut,
          expected_revision: revision,
        });
        /* The answer carries the receipt and the stored record, so a write
         * another pane fenced out leaves the row showing what is actually
         * stored and the reader can press again. */
        if (result.status === "ok") {
          setFailed(result.data.receipt.result !== "committed");
          await load();
        } else {
          setFailed(true);
        }
      } catch {
        setFailed(true);
      } finally {
        setSaving(null);
      }
    })();
  };

  return (
    <SettingsSection label={t("settings.meetings.remoteIntelligence.title")}>
      <MeetingLocalEngineSettings />
      <SettingsRow
        label={t("settings.meetings.remoteIntelligence.label")}
        controlId={switchId}
        disabled={!paired}
      >
        <Switch
          id={switchId}
          checked={enabled}
          disabled={
            !paired || isUpdating("meeting_remote_intelligence_enabled")
          }
          onCheckedChange={(next) =>
            void updateSetting("meeting_remote_intelligence_enabled", next)
          }
        />
      </SettingsRow>
      <div className="flex flex-col gap-2 px-6 py-3">
        <Notice live={false}>
          {t("settings.meetings.remoteIntelligence.consent")}
        </Notice>
        {paired ? null : (
          <Notice tone="warning" live={false}>
            {t("settings.meetings.remoteIntelligence.unpaired")}
          </Notice>
        )}
        {failed ? (
          <Notice tone="danger" assertive>
            {t("settings.meetings.remoteIntelligence.saveFailed")}
          </Notice>
        ) : null}
      </div>
      {enabled ? (
        <>
          <SettingsRow
            label={t("settings.meetings.remoteIntelligence.seriesTitle")}
            hint={t("settings.meetings.remoteIntelligence.seriesHint")}
          />
          {rows === null ? (
            <div className="px-6 py-3">
              <Notice live={false}>
                {t("settings.meetings.remoteIntelligence.loading")}
              </Notice>
            </div>
          ) : rows.length === 0 ? (
            <div className="px-6 py-3">
              <Notice>
                {t("settings.meetings.remoteIntelligence.seriesEmpty")}
              </Notice>
            </div>
          ) : (
            rows.map((row) => (
              <SeriesRow
                key={row.series_key}
                row={row}
                locale={i18n.language}
                saving={saving === row.series_key}
                onToggle={toggleSeries}
              />
            ))
          )}
        </>
      ) : null}
    </SettingsSection>
  );
};
