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
const DEFAULT_LOCAL_ENDPOINT = "http://127.0.0.1:11434/v1";
const MeetingLocalEngineSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const engineId = useId();
  const baseUrlId = useId();
  const modelId = useId();
  const statusRequest = useRef(0);
  const contextWindowId = useId();
  const engine =
    getSetting("meeting_local_engine") ??
    ({ kind: "apple_intelligence" } satisfies MeetingLocalEngine);
  const saving = isUpdating("meeting_local_engine");
  const [status, setStatus] = useState<MeetingLocalEngineStatus | null>(null);
  const [baseUrl, setBaseUrl] = useState(DEFAULT_LOCAL_ENDPOINT);
  const [model, setModel] = useState("");
  const [contextWindow, setContextWindow] = useState("");

  useEffect(() => {
    if (engine.kind !== "local_endpoint") return;
    setBaseUrl(engine.base_url);
    setModel(engine.model);
    setContextWindow(engine.context_window_tokens?.toString() ?? "");
  }, [
    engine.kind,
    engine.kind === "local_endpoint" ? engine.base_url : "",
    engine.kind === "local_endpoint" ? engine.model : "",
    engine.kind === "local_endpoint"
      ? (engine.context_window_tokens ?? null)
      : null,
  ]);

  useEffect(() => {
    const request = ++statusRequest.current;
    setStatus(null);
    if (saving) return;

    void (async () => {
      try {
        const next = await commands.meetingLocalEngineStatus();
        if (request === statusRequest.current) setStatus(next);
      } catch {
        if (request === statusRequest.current) setStatus(null);
      }
    })();
  }, [
    engine.kind,
    engine.kind === "local_endpoint" ? engine.base_url : "",
    engine.kind === "local_endpoint" ? engine.model : "",
    engine.kind === "local_endpoint"
      ? (engine.context_window_tokens ?? null)
      : null,
    saving,
  ]);
  const selectEngine = (value: string) => {
    if (value === engine.kind) return;
    if (value === "apple_intelligence") {
      void updateSetting("meeting_local_engine", {
        kind: "apple_intelligence",
      });
      return;
    }
    void updateSetting("meeting_local_engine", {
      kind: "local_endpoint",
      base_url:
        engine.kind === "local_endpoint"
          ? engine.base_url
          : DEFAULT_LOCAL_ENDPOINT,
      model: engine.kind === "local_endpoint" ? engine.model : "",
      context_window_tokens:
        engine.kind === "local_endpoint"
          ? (engine.context_window_tokens ?? null)
          : null,
    });
  };

  const commitBaseUrl = () => {
    if (engine.kind !== "local_endpoint") return;
    const next = baseUrl.trim();
    if (!next || next === engine.base_url) return;
    void updateSetting("meeting_local_engine", {
      kind: "local_endpoint",
      base_url: next,
      model: engine.model,
      context_window_tokens: engine.context_window_tokens ?? null,
    });
  };

  const commitModel = () => {
    if (engine.kind !== "local_endpoint") return;
    const next = model.trim();
    if (next === engine.model) return;
    void updateSetting("meeting_local_engine", {
      kind: "local_endpoint",
      base_url: engine.base_url,
      model: next,
      context_window_tokens: engine.context_window_tokens ?? null,
    });
  };

  const commitContextWindow = () => {
    if (engine.kind !== "local_endpoint") return;
    const next = contextWindow.trim();
    const value = next === "" ? null : Number(next);
    if (value !== null && (!Number.isSafeInteger(value) || value < 1)) {
      return;
    }
    if (value === (engine.context_window_tokens ?? null)) return;
    void updateSetting("meeting_local_engine", {
      kind: "local_endpoint",
      base_url: engine.base_url,
      model: engine.model,
      context_window_tokens: value,
    });
  };

  const statusTone =
    status?.kind === "apple_intelligence"
      ? status.available
        ? "muted"
        : "warning"
      : status?.kind === "local_endpoint"
        ? status.reachable && status.error === null
          ? "muted"
          : "warning"
        : "muted";
  const statusText =
    status === null
      ? t("settings.meetings.localEngine.status.checking")
      : status.kind === "apple_intelligence"
        ? status.available
          ? t("settings.meetings.localEngine.status.appleAvailable")
          : t("settings.meetings.localEngine.status.appleUnavailable")
        : status.reachable
          ? status.error
            ? status.model_count > 0
              ? t("settings.meetings.localEngine.status.endpointContextUnknown")
              : status.error
            : t("settings.meetings.localEngine.status.endpointReachable", {
                count: status.model_count,
              })
          : t("settings.meetings.localEngine.status.endpointUnreachable", {
              error:
                status.error ??
                t("settings.meetings.localEngine.status.unknownError"),
            });

  return (
    <>
      <SettingsRow
        label={t("settings.meetings.localEngine.label")}
        hint={t("settings.meetings.localEngine.description")}
        controlId={engineId}
        disabled={saving}
      >
        <Select
          value={engine.kind}
          onValueChange={selectEngine}
          disabled={saving}
        >
          <SelectTrigger id={engineId} size="sm" className="w-full">
            <SelectValue>
              {engine.kind === "apple_intelligence"
                ? t("settings.meetings.localEngine.apple")
                : t("settings.meetings.localEngine.endpoint")}
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
      {engine.kind === "local_endpoint" ? (
        <>
          <SettingsField
            label={t("settings.meetings.localEngine.baseUrl.label")}
            hint={t("settings.meetings.localEngine.baseUrl.hint")}
            controlId={baseUrlId}
          >
            <Input
              id={baseUrlId}
              value={baseUrl}
              onChange={(event) => setBaseUrl(event.target.value)}
              onBlur={commitBaseUrl}
              disabled={saving}
              placeholder={DEFAULT_LOCAL_ENDPOINT}
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
              onChange={(event) => setModel(event.target.value)}
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
              onChange={(event) => setContextWindow(event.target.value)}
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
