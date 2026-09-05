import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Trash2 } from "lucide-react";
import { Switch } from "@/components/vg/switch";
import { Notice, SettingsDisclosure } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Checkbox } from "@/components/vg/checkbox";
import { Input } from "@/components/vg/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { useDetectionEditor } from "./MeetingDetectionSettings";
import { summarizeNames } from "./meetingUtils";

/* The apps detection knows by name.
 *
 * `writes` is what ticking the box stores, and it is exactly what the backend
 * seeds itself with (src-tauri/src/meeting/detection/apps.rs
 * DEFAULT_MEETING_APP_BUNDLE_IDS) — Teams ships two identifiers because
 * Microsoft renamed it once and both builds are in the wild. `matches` is
 * wider than `writes` only where the activation observer recognises further
 * identifiers for the same product (meeting_macos.rs), so a list carrying one
 * of those still reads as that app being on rather than as a stray entry.
 *
 * FaceTime and Phone are the two call apps: they never carry a calendar event,
 * their meetings are calls, and they are the only apps whose standing grant
 * anything reads. The backend consults `detection_auto_record_apps` for a
 * `CallSignal` alone (apps.rs CALL_APP_BUNDLE_IDS), so a grant stored for
 * Zoom would round-trip, read back as on, and record nothing. `call` is what
 * keeps the switch off the rows it cannot affect. Browsers are absent on
 * purpose: the browser path is a frontmost-tab reading, not an allowlist
 * entry, which is what the caption under the list says. */
interface KnownMeetingApp {
  /** Names the translation key and the checkbox id; never shown raw. */
  id: string;
  writes: readonly string[];
  matches: readonly string[];
  /** True for the apps `detection_auto_record_apps` is read for. */
  call?: true;
}

const KNOWN_MEETING_APPS: readonly KnownMeetingApp[] = [
  { id: "zoom", writes: ["us.zoom.xos"], matches: ["us.zoom.xos"] },
  {
    id: "teams",
    writes: ["com.microsoft.teams2", "com.microsoft.teams"],
    matches: ["com.microsoft.teams2", "com.microsoft.teams"],
  },
  {
    id: "webex",
    writes: ["com.webex.meetingmanager"],
    matches: [
      "com.webex.meetingmanager",
      "com.cisco.webex",
      "com.cisco.webexmeetingsapp",
    ],
  },
  {
    id: "facetime",
    writes: ["com.apple.facetime"],
    matches: ["com.apple.facetime"],
    call: true,
  },
  {
    id: "phone",
    writes: ["com.apple.mobilephone"],
    matches: ["com.apple.mobilephone"],
    call: true,
  },
  {
    id: "slack",
    writes: ["com.tinyspeck.slackmacgap"],
    matches: ["com.tinyspeck.slackmacgap"],
  },
];

const KNOWN_BUNDLE_IDS: readonly string[] = KNOWN_MEETING_APPS.flatMap(
  (app) => app.matches,
);

/* The format the backend normalises to: trimmed, lowercased, one identifier
 * per entry (apps.rs `normalize_allowlist`). Validating the same shape here is
 * what lets the add sheet refuse a typo instead of storing an inert entry. */
const BUNDLE_ID_PATTERN = /^[a-z0-9][a-z0-9-]*(\.[a-z0-9][a-z0-9-]*)+$/;

interface AddAppSheetProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  existing: readonly string[];
  onAdd: (bundleId: string) => void;
}

/* Adding an app by identifier.
 *
 * This is a typed identifier rather than a file picker on purpose: the app
 * bundle's Info.plist is the only place its identifier lives, and nothing this
 * frontend can reach reads a file — the build grants `dialog:default` but no
 * filesystem permission, and no Tauri command returns a bundle's identifier.
 * A picker that returned "/Applications/Zoom.app" and then still demanded the
 * identifier would be an affordance that does not do its job. */
const AddAppSheet: React.FC<AddAppSheetProps> = ({
  open,
  onOpenChange,
  existing,
  onAdd,
}) => {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");
  const bundleId = draft.trim().toLowerCase();
  const malformed = bundleId.length > 0 && !BUNDLE_ID_PATTERN.test(bundleId);
  const duplicate = existing.includes(bundleId);
  const error = malformed
    ? t("settingsV2.apps.invalid")
    : duplicate
      ? t("settingsV2.apps.duplicate")
      : null;

  const submit = () => {
    if (bundleId.length === 0 || error !== null) return;
    onAdd(bundleId);
    setDraft("");
    onOpenChange(false);
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setDraft("");
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("settingsV2.apps.addTitle")}</DialogTitle>
          <DialogDescription>
            {t("settingsV2.apps.addDescription")}
          </DialogDescription>
        </DialogHeader>
        <form
          className="flex flex-col gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            submit();
          }}
        >
          <label
            htmlFor="detection-add-app"
            className="text-[14px] text-gray-1000"
          >
            {t("settingsV2.apps.identifier")}
          </label>
          <Input
            id="detection-add-app"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder={t("settingsV2.apps.identifierPlaceholder")}
            autoComplete="off"
            spellCheck={false}
            aria-invalid={error !== null}
          />
          {error === null ? null : (
            <Notice tone="danger" assertive>
              {error}
            </Notice>
          )}
        </form>
        <DialogFooter showCloseButton>
          <Button
            type="button"
            onClick={submit}
            disabled={bundleId.length === 0 || error !== null}
          >
            {t("settingsV2.apps.add")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

/* Which applications count as a meeting, as a list of names rather than a
 * textarea of reverse-DNS identifiers.
 *
 * The stored value is unchanged — `meetingApps` is still the array of
 * lowercased bundle IDs `detection_settings_set` takes, written whole through
 * the shared editor. What changed is that the five products Sona already
 * recognises are boxes, and an identifier is only ever typed for the sixth.
 *
 * A disclosure now, because this list is set once and then read never. The
 * summary answers the only question a reader brings to it — is the app I call
 * from covered — and the checklist is opened by whoever finds that answer
 * wrong. Six checkboxes, two switches, three sentences and an Add button were
 * a third of Essentials, in service of a decision most people make on the day
 * they install Sona. */
export const MeetingAppsPicker: React.FC = () => {
  const { t } = useTranslation();
  const { status, settings, saving, patch } = useDetectionEditor();
  const [adding, setAdding] = useState(false);

  const label = t("settingsV2.apps.label");
  const meetingApps = settings?.meetingApps ?? [];
  const running = status?.runningMeetingApps ?? [];
  const disabled = settings === null || !settings.enabled || saving;
  /* An entry nobody put behind a name: a renamed vendor identifier, or one
   * added here. It keeps its own row so removing it does not mean editing
   * a text blob. */
  const custom = meetingApps.filter(
    (bundleId) => !KNOWN_BUNDLE_IDS.includes(bundleId),
  );

  const autoRecordApps = settings?.autoRecordApps ?? [];

  /* What the summary says, in the order the list is drawn: the products that
   * are ticked, then whatever was typed. How many of them fit beside a label
   * is `summarizeNames`' business, and the tracker list's too. */
  const listed = [
    ...KNOWN_MEETING_APPS.filter((app) =>
      app.matches.some((bundleId) => meetingApps.includes(bundleId)),
    ).map((app) => t("settingsV2.apps.names." + app.id)),
    ...custom,
  ];
  const fact =
    /* Nothing read yet is not the same answer as nothing listed, so an
     * unresolved editor states neither. */
    settings === null ? undefined : listed.length === 0 ? (
      /* An empty allowlist cannot ever match an app, and only the reader can
       * fix that — which is the whole test for printing a word in bronze. */
      <span className="text-accent-strong">{t("common.none")}</span>
    ) : (
      summarizeNames(listed)
    );

  const write = (next: readonly string[]) =>
    void patch({ meetingApps: [...next] });

  /* Un-listing an app takes its standing grant with it. A grant naming an app
   * detection no longer watches authorizes nothing, and leaving it behind
   * would bring auto-recording back the moment the box was ticked again. */
  const dropApp = (bundleIds: readonly string[]) =>
    void patch({
      meetingApps: meetingApps.filter((entry) => !bundleIds.includes(entry)),
      autoRecordApps: autoRecordApps.filter(
        (entry) => !bundleIds.includes(entry),
      ),
    });

  return (
    /* Not `lazy`: a closed disclosure costs a reader one line whether the
     * checklist exists yet or not, and deferring it would put the one guard
     * that matters here - an auto-record switch on call apps only - out of
     * reach of a static render. */
    <SettingsDisclosure
      label={label}
      fact={fact}
      disabled={settings !== null && !settings.enabled}
    >
      <div className="flex flex-col gap-3 px-6 py-3.5">
        <ul role="list" aria-label={label} className="flex flex-col gap-2">
          {KNOWN_MEETING_APPS.map((app) => {
            const checked = app.matches.some((bundleId) =>
              meetingApps.includes(bundleId),
            );
            const isRunning = app.matches.some((bundleId) =>
              running.includes(bundleId),
            );
            const name = t("settingsV2.apps.names." + app.id);
            const autoRecord = app.matches.some((bundleId) =>
              autoRecordApps.includes(bundleId),
            );
            return (
              <li key={app.id} className="flex items-center gap-2.5">
                <Checkbox
                  id={"detection-app-" + app.id}
                  checked={checked}
                  disabled={disabled}
                  onCheckedChange={(next) =>
                    next === true
                      ? write([
                          ...meetingApps,
                          ...app.writes.filter(
                            (bundleId) => !meetingApps.includes(bundleId),
                          ),
                        ])
                      : dropApp(app.matches)
                  }
                />
                <label
                  htmlFor={"detection-app-" + app.id}
                  className="text-[14px] text-gray-1000"
                >
                  {name}
                </label>
                {/* The fact that keeps an allowlist honest: an entry only ever
                 * becomes evidence while that application is running. Meta,
                 * not body: it is a reading about the row, not the row. */}
                {isRunning ? (
                  <span className="text-[12px] leading-4 text-gray-900">
                    {t("settingsV2.apps.runningNow")}
                  </span>
                ) : null}
                {app.call ? (
                  <>
                    <label
                      htmlFor={"detection-auto-" + app.id}
                      className="ml-auto text-[14px] text-gray-900"
                    >
                      {t("settingsV2.apps.autoRecord")}
                    </label>
                    <Switch
                      id={"detection-auto-" + app.id}
                      checked={autoRecord}
                      disabled={disabled || !checked}
                      onCheckedChange={(next) =>
                        void patch({
                          autoRecordApps:
                            next === true
                              ? [
                                  ...autoRecordApps,
                                  ...app.writes.filter(
                                    (bundleId) =>
                                      !autoRecordApps.includes(bundleId),
                                  ),
                                ]
                              : autoRecordApps.filter(
                                  (bundleId) => !app.matches.includes(bundleId),
                                ),
                        })
                      }
                    />
                  </>
                ) : null}
              </li>
            );
          })}
          {custom.map((bundleId) => (
            <li key={bundleId} className="flex items-center gap-2.5">
              <Checkbox
                id={"detection-app-" + bundleId}
                checked
                disabled={disabled}
                onCheckedChange={() => dropApp([bundleId])}
              />
              <label
                htmlFor={"detection-app-" + bundleId}
                className="min-w-0 truncate text-[14px] text-gray-1000"
              >
                {bundleId}
              </label>
              {running.includes(bundleId) ? (
                <span className="text-[12px] leading-4 text-gray-900">
                  {t("settingsV2.apps.runningNow")}
                </span>
              ) : null}
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                className="ml-auto text-red-900"
                aria-label={t("settingsV2.apps.remove", { app: bundleId })}
                disabled={disabled}
                onClick={() => dropApp([bundleId])}
              >
                <Trash2 aria-hidden="true" />
              </Button>
            </li>
          ))}
        </ul>
        {/* One sentence, and it is about the switch rather than the list: what
         * "Record automatically" spends is the only thing in here a reader
         * cannot get from the labels. The consent receipt a standing app grant
         * writes (session.rs start_from_standing_app) acknowledges both
         * sources, and it may only claim what was on screen when the switch
         * was flipped: this sentence is that screen. */}
        <Notice tone="muted" live={false}>
          {t("settingsV2.apps.autoRecordSources")}
        </Notice>
        <div className="flex justify-end">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled}
            onClick={() => setAdding(true)}
          >
            <Plus aria-hidden="true" />
            {t("settingsV2.apps.add")}
          </Button>
        </div>
        <AddAppSheet
          open={adding}
          onOpenChange={setAdding}
          existing={meetingApps}
          onAdd={(bundleId) => write([...meetingApps, bundleId])}
        />
      </div>
    </SettingsDisclosure>
  );
};
