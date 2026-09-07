import { Suspense, useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { platform } from "@tauri-apps/plugin-os";
import {
  checkAccessibilityPermission,
  checkMicrophonePermission,
} from "tauri-plugin-macos-permissions-api";
import { ModelStateEvent, RecordingErrorEvent } from "./lib/types/events";
import "./App.css";
import AccessibilityPermissions from "./components/AccessibilityPermissions";
import SecureInputWarning from "./components/SecureInputWarning";
import Onboarding from "./components/onboarding/Onboarding";
import AccessibilityOnboarding from "./components/onboarding/AccessibilityOnboarding";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { LaunchShell } from "./components/LaunchShell";
import { Sidebar } from "./components/Sidebar";
import {
  ChatOpenerProvider,
  ChatSheetHost,
} from "./components/chat/ChatSheetHost";
import { CommandPalette } from "./components/CommandPalette";
import { DetectionListeners } from "./components/settings/meetings/DetectionListeners";
import { PAGE_COLUMN } from "./components/settings/rows";
import { RouteSkeleton } from "./components/RouteSkeleton";
import { Toaster } from "./components/Toaster";
import { RecorderDialog } from "./components/recorder/RecorderDialog";
import { ImportDialogHost } from "./components/import/ImportDialog";
import {
  isCommandPaletteChord,
  type CommandPaletteAction,
} from "./components/commandPaletteActions";
import {
  buildNavigationActions,
  SECTIONS_CONFIG,
  type SidebarSection,
} from "./components/sidebarSections";
import { usePromptShellStore } from "./components/settings/meetings/promptTargets";
import {
  DEFAULT_SETTINGS_TARGET,
  nextSettingsNavigationRequest,
  type SettingsNavigationRequest,
  type SettingsNavigationTarget,
} from "./components/settings/navigation";
import type { DictationRequest } from "./components/settings/history/HistorySettings";
import { WhatsNewGate } from "./components/whats-new";
import { useAudioImport } from "./hooks/useAudioImport";
import { useMeetingImport } from "./hooks/useMeetingImport";
import { useShellTravel } from "./hooks/useShellTravel";
import { useSettings } from "./hooks/useSettings";
import { useSettingsStore } from "./stores/settingsStore";
import { commands, events, type MeetingNavigationPayload } from "@/bindings";
import { cn } from "@/lib/cn";
import {
  getLanguageDirection,
  initializeRTL,
  type LanguageDirection,
} from "@/lib/utils/rtl";
import { runViewTransition } from "@/lib/utils/viewTransition";
import { MotionProvider } from "@/lib/motion/provider";

type OnboardingStep = "accessibility" | "model" | "done";

/* One `sona://` address the shell was asked to open, as the surface that owns
 * it needs it. The nonce is the request, not the target: asking for the same
 * person or the same question twice has to move state twice, and a bare id
 * would be equal to the one already there. */
interface PersonRequest {
  personId: string;
  nonce: number;
}

interface OrganizationRequest {
  slug: string;
  nonce: number;
}

interface SearchRequest {
  query: string;
  nonce: number;
}

interface SettingsContentProps {
  section: SidebarSection;
  meetingInvalidation: number;
  meetingNavigationRequest: MeetingNavigationPayload | null;
  meetingStartRequest: number;
  personRequest: PersonRequest | null;
  organizationRequest: OrganizationRequest | null;
  dictationRequest: DictationRequest | null;
  settingsNavigationRequest?: SettingsNavigationRequest | null;
  onSectionChange: (
    section: SidebarSection,
    target?: SettingsNavigationTarget,
  ) => void;
  onOpenMeeting: (meetingId: string) => void;
  onOpenRecorder: () => void;
}

const renderSettingsContent = ({
  section,
  meetingInvalidation,
  meetingNavigationRequest,
  meetingStartRequest,
  personRequest,
  organizationRequest,
  dictationRequest,
  settingsNavigationRequest,
  onSectionChange,
  onOpenMeeting,
  onOpenRecorder,
}: SettingsContentProps) => {
  if (section === "overview") {
    const OverviewComponent = SECTIONS_CONFIG.overview.component;
    return (
      <OverviewComponent
        onOpenSection={onSectionChange}
        onOpenMeeting={onOpenMeeting}
        onOpenRecorder={onOpenRecorder}
      />
    );
  }

  if (section === "history") {
    const HistoryComponent = SECTIONS_CONFIG.history.component;
    return <HistoryComponent dictationRequest={dictationRequest} />;
  }

  if (section === "meetings") {
    const MeetingsComponent = SECTIONS_CONFIG.meetings.component;
    return (
      <MeetingsComponent
        invalidation={meetingInvalidation}
        navigationRequest={meetingNavigationRequest}
        startRequest={meetingStartRequest}
        onOpenSettings={() =>
          onSectionChange("settings", { tab: "advanced", section: "meetings" })
        }
      />
    );
  }

  if (section === "people") {
    const PeopleComponent = SECTIONS_CONFIG.people.component;
    return (
      <PeopleComponent
        onOpenMeeting={onOpenMeeting}
        personRequest={personRequest}
        organizationRequest={organizationRequest}
      />
    );
  }

  if (section === "models") {
    const ModelsComponent = SECTIONS_CONFIG.models.component;
    return (
      <ModelsComponent
        onOpenPrompts={() => onSectionChange("settings", { tab: "prompts" })}
      />
    );
  }
  if (section === "settings") {
    const SettingsComponent = SECTIONS_CONFIG.settings.component;
    /* Essentials and Advanced each hold one link row — dictation styles and
     * the model catalog — and both are destinations the rail no longer lists.
     * Routing is this component's job, so the hub gets the same callback the
     * rail and the palette use rather than a second way to change the view. */
    return (
      <SettingsComponent
        navigationRequest={settingsNavigationRequest}
        onOpenSection={onSectionChange}
      />
    );
  }

  const ActiveComponent =
    SECTIONS_CONFIG[section]?.component || SECTIONS_CONFIG.overview.component;
  return <ActiveComponent />;
};

const subscribeToMeetingEvents = async (
  invalidate: () => void,
  navigate: (payload: MeetingNavigationPayload) => void,
) => {
  const unlisten = await Promise.all([
    events.meetingSuggestionChanged.listen(invalidate),
    events.meetingSessionChanged.listen(invalidate),
    events.meetingSourceHealthChanged.listen(invalidate),
    events.meetingTranscriptChanged.listen(invalidate),
    events.meetingNoteChanged.listen(invalidate),
    events.meetingArtifactChanged.listen(invalidate),
    events.meetingRemoteJobChanged.listen(invalidate),
    events.meetingRemoved.listen(invalidate),
    events.meetingNavigationRequested.listen((event) => {
      navigate(event.payload);
      invalidate();
    }),
  ]);

  return async () => {
    await Promise.all(unlisten.map((listener) => listener()));
  };
};

/* The toast surface and the route skeleton are app components, not kit
 * primitives — they own this app's copy rules and page rhythm. The shell only
 * decides where they mount. */

export interface AppContentProps {
  onboardingStep: OnboardingStep | null;
  onAccessibilityComplete: () => void;
  onModelSelected: () => void;
  direction: LanguageDirection;
  currentSection: SidebarSection;
  onSectionChange: (
    section: SidebarSection,
    target?: SettingsNavigationTarget,
  ) => void;
  onOpenMeeting: (meetingId: string) => void;
  onOpenRecorder: () => void;
  loadingLabel: string;
  meetingInvalidation: number;
  meetingNavigationRequest: MeetingNavigationPayload | null;
  meetingStartRequest: number;
  personRequest: PersonRequest | null;
  organizationRequest: OrganizationRequest | null;
  dictationRequest: DictationRequest | null;
  settingsNavigationRequest?: SettingsNavigationRequest | null;
  commandOpen: boolean;
  commandActions: CommandPaletteAction[];
  commandSeed: SearchRequest | null;
  agentPanel: {
    enabled: boolean;
    paired: boolean;
    remoteIntelligence: boolean;
  };
  /** The chat sheet's fold, owned here so the rail, Esc and ⌘K agree on it. */
  chatOpen: boolean;
  onChatOpenChange: (open: boolean) => void;
  onCommandOpenChange: (open: boolean) => void;
  onCommandOpen: () => void;
}

/* Exported for the shell's own test. While the onboarding probe is pending,
 * this paints the real frame geometry with a route skeleton; the backend model
 * catalog may still be loading without leaving an empty native window. */
export const AppContent = ({
  onboardingStep,
  onAccessibilityComplete,
  onModelSelected,
  direction,
  currentSection,
  onSectionChange,
  onOpenMeeting,
  onOpenRecorder,
  loadingLabel,
  meetingInvalidation,
  meetingNavigationRequest,
  meetingStartRequest,
  personRequest,
  organizationRequest,
  dictationRequest,
  settingsNavigationRequest,
  commandOpen,
  commandActions,
  commandSeed,
  agentPanel,
  chatOpen,
  onChatOpenChange,
  onCommandOpenChange,
  onCommandOpen,
}: AppContentProps) => {
  /* One fold, three columns. The chat is a column of the window rather than a
   * strip over the page, so opening it collapses the rail to glyphs and
   * narrows the page instead of covering it: 48 + 512 + 340 of the same fixed
   * 900, and nothing anywhere resizes the window.
   *
   * The setting is folded in here rather than at each reader, so the rail's
   * chat row and the column cannot disagree about whether it is showing: an
   * agent switched off has no column, and a page beside a column that is not
   * there would be a page narrowed for nothing. */
  const chatShowing = chatOpen && agentPanel.enabled;

  /* The shell's one clock, above the onboarding returns because a hook has to
   * be. Nothing travels during onboarding — there is no chat to open — so this
   * is inert until the shell below is the thing on screen. */
  const travel = useShellTravel(chatShowing);

  if (onboardingStep === null) {
    return <LaunchShell direction={direction} loadingLabel={loadingLabel} />;
  }
  if (onboardingStep === "accessibility") {
    return <AccessibilityOnboarding onComplete={onAccessibilityComplete} />;
  }
  if (onboardingStep === "model") {
    return <Onboarding onModelSelected={onModelSelected} />;
  }

  /* Anything that wants the chat from below — a review's follow-up button, the
   * palette's Ask row — reaches the same fold the rail's row opens rather than
   * opening a second one. */
  return (
    <ChatOpenerProvider value={() => onChatOpenChange(true)}>
      <div
        dir={direction}
        /* `app-shell` carries no styling of its own any more. It survives as
         * the hook the two shell rules in styles/shell.css key off: the Glass
         * material override, which reads a root attribute Rust writes, and the
         * travel gate below, which reaches down into the column and the rail.
         * Neither is a thing a utility on the element can say.
         *
         * `data-shell-open` changes the root's registered CSS properties. That
         * root is the one transition owner: its clock moves the fixed frame
         * and crossfades the old, decorative rail against the new fixed-width
         * one. `data-shell-moving` exists only while that clock runs. It puts
         * `will-change` on the moving frame, holds inner transitions still, and
         * lets the outgoing rail exist for the same interval. `transitionend`
         * bubbles to here because the shell owns the transition. */
        data-shell-open={chatShowing ? "true" : undefined}
        data-shell-moving={travel.moving ? "true" : undefined}
        data-shell-direction={
          travel.moving ? (chatShowing ? "opening" : "closing") : undefined
        }
        onTransitionEnd={travel.onTransitionEnd}
        className="app-shell relative flex h-screen cursor-default select-none bg-background-200"
      >
        <ErrorBoundary context="What's New">
          <WhatsNewGate />
        </ErrorBoundary>
        <Sidebar
          collapsed={chatShowing}
          currentSection={currentSection}
          onSectionChange={onSectionChange}
          onOpenCommand={onCommandOpen}
          agentPanel={agentPanel}
          chatOpen={chatShowing}
          onOpenChat={() => onChatOpenChange(true)}
        />
        {travel.moving && (
          /* The rail's outgoing form is visual only. It overlays the structural
             rail at its old fixed width and fades through the shell's same
             custom-property clock, while `inert` keeps duplicate buttons and
             drag regions out of every interaction path. */
          <Sidebar
            collapsed={!chatShowing}
            currentSection={currentSection}
            onSectionChange={onSectionChange}
            onOpenCommand={onCommandOpen}
            agentPanel={agentPanel}
            chatOpen={chatShowing}
            onOpenChat={() => onChatOpenChange(true)}
            dataSlot="sidebar-ghost"
            decorative
            className="pointer-events-none absolute inset-y-0 start-0 z-30"
          />
        )}
        {/* `settings-main` is a hook as well: primitives.css still styles bare
         * inputs and selects through it for the surfaces that have not moved to
         * the component kit yet. `relative` is what the drag band below is
         * positioned against: the content pane, not the page and not the
         * scroll box. Nothing else floats in this pane — the chat's own door
         * is a rail row (Sidebar.tsx), because the corner the pane's top band
         * leaves empty is exactly where every page puts its primary action. */}
        <main className="settings-main relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden transition-none">
          {/* The window's drag handle, and the reason this app can be moved at
           * all on macOS: the window is TitleBarStyle::Overlay with a hidden
           * title, so the webview covers the native title bar and the traffic
           * lights are the only native thing left up there. Every pixel the
           * cursor can grab has to be claimed in markup. The rail claims its own
           * top strip (Sidebar.tsx); this claims the pane's, which is the band
           * the reader actually reaches for — the full width above page content,
           * on the traffic lights' row.
           *
           * `h-12` rather than a pinned 42px: this band IS the `py-12` every
           * page leaves above its first heading, and writing it as the same step
           * on the same scale is what stops the two from drifting apart.
           *
           * Unconditional. `data-tauri-drag-region` is inert on a platform that
           * kept its native title bar, so gating it on macOS would buy a branch
           * and nothing else.
           *
           * Bare, not `deep`: Tauri only starts a drag when the mousedown lands
           * on this element itself, and it has no children. `z-0` keeps it under
           * anything a page positions over it, and it holds no interactive
           * control of its own. */}
          <div
            data-slot="drag-band"
            data-tauri-drag-region
            className="absolute inset-x-0 top-0 z-0 h-12"
          />
          <div
            data-slot="page-scroll"
            className="flex-1 min-h-0 overflow-x-hidden overflow-y-auto transition-none"
          >
            {/* Every page owns its own column now, so the scroll region is full
             * width and unpadded. These two are shell banners rather than
             * pages, so they borrow the pages' column from the primitive that
             * owns it — and both render nothing on the ordinary path, which is
             * what collapses the wrapper. The top padding matches a page's
             * `py-12` rather than sitting 14px above it: the drag band is the
             * one thing in the pane a banner may not grow into, because a
             * banner over it is a window nobody can move. */}
            <div className={cn(PAGE_COLUMN, "pt-12 empty:hidden")}>
              <AccessibilityPermissions />
              <SecureInputWarning />
            </div>
            {/* Keyed on the section so switching routes shows the skeleton
             * again rather than holding the previous page while the next
             * chunk loads, and so a crashed section resets when you leave. */}
            <ErrorBoundary key={currentSection} context={currentSection}>
              <Suspense
                fallback={
                  <div className={cn(PAGE_COLUMN, "py-12")}>
                    <RouteSkeleton label={loadingLabel} />
                  </div>
                }
              >
                {renderSettingsContent({
                  section: currentSection,
                  meetingInvalidation,
                  meetingNavigationRequest,
                  meetingStartRequest,
                  personRequest,
                  organizationRequest,
                  dictationRequest,
                  settingsNavigationRequest,
                  onSectionChange,
                  onOpenMeeting,
                  onOpenRecorder,
                })}
              </Suspense>
            </ErrorBoundary>
          </div>
        </main>
        {/* A column of the shell, beside the pane rather than over it — which
         * is the whole cutover: the page it is asked about stays lit, readable
         * and clickable at 512pt, and the window never changes size. The
         * setting closes it rather than hiding a column that is still open: an
         * agent switched off has no conversation to be mid-way through. */}
        <ChatSheetHost
          open={chatShowing}
          panel={agentPanel}
          onClose={() => onChatOpenChange(false)}
          onOpenSettings={() =>
            onSectionChange("settings", {
              tab: "advanced",
              section: "sonaAgent",
            })
          }
        />
        <CommandPalette
          open={commandOpen}
          onOpenChange={onCommandOpenChange}
          actions={commandActions}
          seed={commandSeed}
          panel={agentPanel}
          onAsk={() => onChatOpenChange(true)}
        />
      </div>
    </ChatOpenerProvider>
  );
};

interface CommandActionDeps {
  t: (key: string) => string;
  isMacos: boolean;
  agentEnabled: boolean;
  onNavigate: (
    section: SidebarSection,
    target?: SettingsNavigationTarget,
  ) => void;
  onNewMeeting: () => void;
  onImportAudio: () => void;
  onImportMeeting: () => void;
  onOpenRecordings: () => void;
  onOpenAgent: () => void;
  onOpenRecorder: () => void;
}

export const buildCommandActions = ({
  t,
  agentEnabled,
  isMacos,
  onNavigate,
  onNewMeeting,
  onImportAudio,
  onImportMeeting,
  onOpenRecordings,
  onOpenAgent,
  onOpenRecorder,
}: CommandActionDeps): CommandPaletteAction[] => [
  /* The destinations come from the section registry, in the one order it lists
   * them: the rail takes the same list, so neither surface can rename or
   * reorder a destination on its own. Models is last because the registry lists
   * it last, and has no rail row because the registry says `inRail: false`. */
  ...buildNavigationActions(t, onNavigate),
  {
    id: "action-meeting",
    group: "actions",
    label: t("commandPalette.newMeeting"),
    run: onNewMeeting,
  },
  {
    id: "action-import",
    group: "actions",
    label: t("commandPalette.importAudio"),
    run: onImportAudio,
  },
  ...(isMacos
    ? [
        {
          id: "action-screen-recording",
          group: "actions" as const,
          label: t("recorder.open"),
          run: onOpenRecorder,
        },
      ]
    : []),
  {
    id: "action-import-meeting",
    group: "actions",
    label: t("commandPalette.importMeeting"),
    run: onImportMeeting,
  },
  {
    id: "action-recordings",
    group: "actions",
    label: t("commandPalette.openRecordings"),
    run: onOpenRecordings,
  },
  {
    id: "action-new-prompt",
    group: "actions",
    label: t("commandPalette.newPrompt"),
    /* Settings is where prompts live, and the editor opens on arrival: an
     * action named "New prompt" that only scrolled you near one would be a
     * second press before anything happened. */
    run: () => {
      onNavigate("settings", { tab: "prompts" });
      usePromptShellStore.getState().requestNewPrompt();
    },
  },
  ...(agentEnabled
    ? [
        {
          id: "action-agent",
          group: "actions" as const,
          label: t("commandPalette.openAgent"),
          run: onOpenAgent,
        },
      ]
    : []),
];

const AppEventListeners: React.FC = () => {
  const { t } = useTranslation();
  // Listen for recording errors from the backend and show a toast
  useEffect(() => {
    const unlisten = listen<RecordingErrorEvent>("recording-error", (event) => {
      const { error_type, detail } = event.payload;

      /* One sentence each: the cause, then the way out. These five used to
         pass a short title with the sentence under it, and the sentence was
         the whole message every time — "No microphone found" over "No audio
         input device was detected. Connect a microphone and try again."
         reads the same fact twice at two sizes. The short forms are still
         the HUD's, where the pill has room for two words and nothing more
         (see overlay/RecordingOverlayContent.tsx). */
      if (error_type === "microphone_permission_denied") {
        const platformKey = `errors.micPermissionDenied.${platform()}`;
        toast.error(
          t(platformKey, {
            defaultValue: t("errors.micPermissionDenied.generic"),
          }),
        );
      } else if (error_type === "no_input_device") {
        toast.error(t("errors.noInputDevice"));
      } else if (error_type === "no_speech_detected") {
        toast.info(t("errors.noSpeechDetected"));
      } else if (error_type === "no_model_selected") {
        toast.error(
          t(
            "errors.noModelSelected",
            "No transcription model selected. Choose one in Settings > Models.",
          ),
        );
      } else if (error_type === "command_no_selection") {
        toast.error(
          t(
            "errors.commandNoSelection",
            "Select the text you want to change, then hold the command shortcut and say the change.",
          ),
        );
      } else if (error_type === "command_rewrite_unavailable") {
        toast.error(
          t(
            "errors.commandRewriteUnavailable",
            "The rewrite returned nothing, so your selection was left as it was. Check the provider in Settings > Post-processing and try again.",
          ),
        );
      } else if (error_type === "no_speech_save_failed") {
        toast.error(
          t("errors.recordingFailed", {
            error: t(
              "settings.advanced.customWords.audioImport.failure.history",
            ),
          }),
        );
      } else {
        toast.error(
          t("errors.recordingFailed", { error: detail ?? "Unknown error" }),
        );
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for paste failures and show a toast.
  // The technical error detail is logged to sona.log on the Rust side
  // (see actions.rs `error!("Failed to paste transcription: ...")`),
  // so we show a localized, user-friendly message here instead of the raw error.
  useEffect(() => {
    const unlisten = listen("paste-error", () => {
      toast.error(t("errors.pasteFailed"));
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | null = null;
    void events.trayCopyFailed
      .listen(() => {
        if (!disposed) toast.error(t("settings.history.copyFailed"));
      })
      .then((cleanup) => {
        if (disposed) cleanup();
        else unsubscribe = cleanup;
      });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [t]);

  /* Listen for transcription failures and show a toast. The payload is a
     fixed English string from the Rust side ("Transcription failed",
     actions.rs), which is the title again in the backend's words rather than
     a detail worth reading; the real detail is in sona.log. */
  useEffect(() => {
    const unlisten = listen("transcription-error", () => {
      toast.error(t("errors.transcriptionFailed"));
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  useEffect(() => {
    const unlisten = listen("audio-import-open-error", () => {
      toast.error(t("settings.advanced.customWords.audioImport.errors.start"));
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  /* Where a file the operating system handed to Sona ended up. Open With
     carries a path and no destination, so this is the one import route that
     has to say which of the two homes the recording went to — and the action
     opens the thing the sentence names. */
  useEffect(() => {
    const unlisten = events.audioImportRoutedEvent.listen((event) => {
      toast.success(
        t(`settings.history.audioImport.routed.${event.payload.destination}`, {
          file: event.payload.file_name,
        }),
        {
          action: {
            label: t("common.open"),
            onClick: () => void commands.sonaOpenLink(event.payload.link),
          },
        },
      );
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for model loading failures and show a toast
  useEffect(() => {
    const unlisten = listen<ModelStateEvent>("model-state-changed", (event) => {
      if (event.payload.event_type === "loading_failed") {
        toast.error(
          t("errors.modelLoadFailed", {
            model:
              event.payload.model_name || t("errors.modelLoadFailedUnknown"),
          }),
          {
            description: event.payload.error,
          },
        );
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  return null;
};

const revealMainWindowForPermissions = async (): Promise<void> => {
  try {
    await commands.showMainWindowCommand();
  } catch (error) {
    console.warn(
      "Failed to show main window for permission onboarding:",
      error,
    );
  }
};

function App() {
  const { t, i18n } = useTranslation();
  const [onboardingStep, setOnboardingStep] = useState<OnboardingStep | null>(
    null,
  );
  // Track if this is a returning user who just needs to grant permissions
  // (vs a new user who needs full onboarding including model selection)
  const isReturningUserRef = useRef(false);
  const [currentSection, setCurrentSection] =
    useState<SidebarSection>("overview");
  const [meetingInvalidation, setMeetingInvalidation] = useState(0);
  const [meetingNavigationRequest, setMeetingNavigationRequest] =
    useState<MeetingNavigationPayload | null>(null);
  const [meetingStartRequest, setMeetingStartRequest] = useState(0);
  const [settingsNavigationRequest, setSettingsNavigationRequest] =
    useState<SettingsNavigationRequest | null>(null);
  /* One modal at a time. The palette and the recorder cannot both be up, so
   * which one is up is a single value rather than two booleans that five
   * guards kept in agreement. `setPalette` is then the only place that says
   * the recorder outranks the palette. */
  const [modal, setModal] = useState<"none" | "palette" | "recorder">("none");
  const commandOpen = modal === "palette";
  const recorderOpen = modal === "recorder";
  const setPalette = useCallback((next: boolean | "toggle") => {
    setModal((current) => {
      if (current === "recorder") return current;
      const open = next === "toggle" ? current !== "palette" : next;
      return open ? "palette" : "none";
    });
  }, []);
  /* Not persisted. The sheet is a thing you opened during this sitting, and a
   * chat that reopens itself on the next launch is a chat that reopens a
   * question you already stopped asking. */
  const [chatOpen, setChatOpen] = useState(false);
  const [commandSeed, setCommandSeed] = useState<SearchRequest | null>(null);
  const [personRequest, setPersonRequest] = useState<PersonRequest | null>(
    null,
  );
  const [organizationRequest, setOrganizationRequest] =
    useState<OrganizationRequest | null>(null);
  const [dictationRequest, setDictationRequest] =
    useState<DictationRequest | null>(null);
  const { settings, updateSetting } = useSettings();
  const direction = getLanguageDirection(i18n.language);
  const refreshAudioDevices = useSettingsStore(
    (state) => state.refreshAudioDevices,
  );
  const refreshOutputDevices = useSettingsStore(
    (state) => state.refreshOutputDevices,
  );
  const hasCompletedPostOnboardingInit = useRef(false);

  /* A route change swaps the whole view, which is the one case the View
   * Transitions API is for. The deep-link handler below deliberately keeps the
   * raw setter: it moves three pieces of state at once, and snapshotting a
   * partial update would tear. */
  const navigateToSection = useCallback(
    (section: SidebarSection, target?: SettingsNavigationTarget) => {
      runViewTransition(() => {
        if (
          section === "settings" &&
          (currentSection !== "settings" || target !== undefined)
        ) {
          setSettingsNavigationRequest((current) =>
            nextSettingsNavigationRequest(
              current,
              target ?? DEFAULT_SETTINGS_TARGET,
            ),
          );
        }
        setCurrentSection(section);
      });
    },
    [currentSection],
  );

  /* Overview meeting links reuse the same navigation payload consumed by deep
   * links. The meetings controller reloads the authoritative snapshot, so zero
   * is the established unknown-revision value rather than a copied snapshot. */
  const openMeeting = useCallback((meetingId: string) => {
    runViewTransition(() => {
      setMeetingNavigationRequest({
        event_schema_version: 1,
        destination: "session",
        session_id: meetingId,
        revision: 0,
      });
      setCurrentSection("meetings");
    });
  }, []);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => Promise<void>) | null = null;
    const invalidateMeetings = () => {
      if (!disposed) {
        setMeetingInvalidation((current) => current + 1);
      }
    };

    void subscribeToMeetingEvents(invalidateMeetings, (payload) => {
      setMeetingNavigationRequest(payload);
      // sona://meeting/start asks for the start surface and carries no session
      // (lib.rs dispatch_deep_link); every other preflight payload names one.
      if (payload.destination === "preflight" && payload.session_id === null) {
        setMeetingStartRequest((current) => current + 1);
      }
      setCurrentSection("meetings");
    }).then((cleanup) => {
      if (disposed) {
        void cleanup();
      } else {
        unsubscribe = cleanup;
      }
    });

    return () => {
      disposed = true;
      if (unsubscribe) {
        void unsubscribe();
      }
    };
  }, []);

  /* The evening digest's one gesture. It names no object — the notification is
   * about the day — so it asks for Capture and nothing else. */
  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | null = null;
    void events.sonaCaptureRequested
      .listen(() => {
        if (!disposed) setCurrentSection("overview");
      })
      .then((cleanup) => {
        if (disposed) cleanup();
        else unsubscribe = cleanup;
      });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | null = null;
    void events.traySettingsRequested
      .listen(() => {
        if (!disposed) navigateToSection("settings");
      })
      .then((cleanup) => {
        if (disposed) cleanup();
        else unsubscribe = cleanup;
      });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [navigateToSection]);

  /* The other half of `sona://`. Meetings and loops arrive on the meeting
   * navigation event above, because a meeting has a lifecycle to navigate; the
   * nouns that had no destination of their own arrive here. One listener, three
   * targets, and the same routing whether the address came from the OS, from a
   * ⌘K row, or from a link an agent cited in the panel.
   *
   * A dictation link carries its stable history id to History. History clears
   * a stale search, pages if needed, expands, scrolls, and focuses that row;
   * this listener only delivers the request. */
  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | null = null;
    void events.queryLinkRequested
      .listen((event) => {
        if (disposed) return;
        const target = event.payload.target;
        if (target.kind === "person") {
          setPersonRequest((current) => ({
            personId: target.person_id,
            nonce: (current?.nonce ?? 0) + 1,
          }));
          runViewTransition(() => setCurrentSection("people"));
          return;
        }
        if (target.kind === "organization") {
          setOrganizationRequest((current) => ({
            slug: target.slug,
            nonce: (current?.nonce ?? 0) + 1,
          }));
          runViewTransition(() => setCurrentSection("people"));
          return;
        }
        if (target.kind === "dictation") {
          setDictationRequest((current) => ({
            historyId: target.history_id,
            nonce: (current?.nonce ?? 0) + 1,
          }));
          runViewTransition(() => setCurrentSection("history"));
          return;
        }
        setCommandSeed((current) => ({
          query: target.query,
          nonce: (current?.nonce ?? 0) + 1,
        }));
        setPalette(true);
      })
      .then((cleanup) => {
        if (disposed) cleanup();
        else unsubscribe = cleanup;
      });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [setPalette]);

  useEffect(() => {
    checkOnboardingStatus();
  }, []);

  // Initialize RTL direction when language changes
  useEffect(() => {
    initializeRTL(i18n.language);
  }, [i18n.language]);

  // Initialize Enigo, shortcuts, and refresh audio devices when main app loads
  useEffect(() => {
    if (onboardingStep === "done" && !hasCompletedPostOnboardingInit.current) {
      hasCompletedPostOnboardingInit.current = true;
      Promise.all([
        commands.initializeEnigo(),
        commands.initializeShortcuts(),
      ]).catch((e) => {
        console.warn("Failed to initialize:", e);
      });
      refreshAudioDevices();
      refreshOutputDevices();
    }
  }, [onboardingStep, refreshAudioDevices, refreshOutputDevices]);

  /* The palette's chord, and the reason `isCommandPaletteChord` is a named
   * predicate: it drops auto-repeats. The chord toggles, and a held key
   * repeats keydown at the OS repeat rate, so this listener used to flip the
   * palette open and shut dozens of times a second for as long as the chord
   * was held. */
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!isCommandPaletteChord(event)) return;
      event.preventDefault();
      setPalette("toggle");
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [setPalette]);

  // Handle keyboard shortcuts for debug mode toggle
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      // Check for Ctrl+Shift+D (Windows/Linux) or Cmd+Shift+D (macOS)
      const isDebugShortcut =
        event.shiftKey &&
        event.key.toLowerCase() === "d" &&
        (event.ctrlKey || event.metaKey);

      if (isDebugShortcut) {
        event.preventDefault();
        const currentDebugMode = settings?.debug_mode ?? false;
        updateSetting("debug_mode", !currentDebugMode);
      }
    };

    // Add event listener when component mounts
    document.addEventListener("keydown", handleKeyDown);

    // Cleanup event listener when component unmounts
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [settings?.debug_mode, updateSetting]);

  const checkOnboardingStatus = async () => {
    try {
      const settingsResult = await commands.getAppSettings();
      const hasCompletedOnboarding =
        settingsResult.status === "ok" &&
        settingsResult.data.onboarding_completed === true;
      const currentPlatform = platform();

      if (hasCompletedOnboarding) {
        // Returning user - check if they need to grant permissions first
        isReturningUserRef.current = true;

        if (currentPlatform === "macos") {
          try {
            const [hasAccessibility, hasMicrophone] = await Promise.all([
              checkAccessibilityPermission(),
              checkMicrophonePermission(),
            ]);
            if (!hasAccessibility || !hasMicrophone) {
              await revealMainWindowForPermissions();
              setOnboardingStep("accessibility");
              return;
            }
          } catch (e) {
            console.warn("Failed to check macOS permissions:", e);
            // If we can't check, proceed to main app and let them fix it there
          }
        }

        if (currentPlatform === "windows") {
          try {
            const microphoneStatus =
              await commands.getWindowsMicrophonePermissionStatus();
            if (
              microphoneStatus.supported &&
              microphoneStatus.overall_access === "denied"
            ) {
              await revealMainWindowForPermissions();
              setOnboardingStep("accessibility");
              return;
            }
          } catch (e) {
            console.warn("Failed to check Windows microphone permissions:", e);
            // If we can't check, proceed to main app and let them fix it there
          }
        }

        setOnboardingStep("done");
      } else {
        // New user - start full onboarding
        isReturningUserRef.current = false;
        setOnboardingStep("accessibility");
      }
    } catch (error) {
      console.error("Failed to check onboarding status:", error);
      setOnboardingStep("accessibility");
    }
  };

  const handleAccessibilityComplete = () => {
    // Returning users already have models, skip to main app
    // New users need to select a model
    setOnboardingStep(isReturningUserRef.current ? "done" : "model");
  };

  const handleModelSelected = () => {
    // Transition to main app - user has started a download
    setOnboardingStep("done");
  };

  const { start: startAudioImport } = useAudioImport();
  /* The palette imports a meeting from wherever the app happens to be, so it
   * routes to the imported meeting the same way an Overview link does. */
  const { start: startMeetingImport } = useMeetingImport({
    onImported: openMeeting,
  });

  const openRecordingsFolder = useCallback(async () => {
    try {
      const result = await commands.openRecordingsFolder();
      if (result.status !== "ok") {
        throw new Error(String(result.error));
      }
    } catch (error) {
      console.error("Failed to open recordings folder:", error);
    }
  }, []);

  const openChat = useCallback(() => setChatOpen(true), []);
  const openRecorder = useCallback(() => setModal("recorder"), []);

  const agentEnabled = settings?.agent_panel_enabled === true;

  const openCommandPalette = useCallback(() => setPalette(true), [setPalette]);

  /* Route changes go through `navigateToSection` here for the same reason the
   * sidebar rows do — the palette and the rail reach the same destinations, so
   * they cannot swap the view two different ways. */
  const commandActions = buildCommandActions({
    t,
    agentEnabled,
    isMacos: platform() === "macos",
    onNavigate: navigateToSection,
    onNewMeeting: () =>
      runViewTransition(() => {
        setCurrentSection("meetings");
        setMeetingStartRequest((current) => current + 1);
      }),
    onImportAudio: () => void startAudioImport(),
    onImportMeeting: () => void startMeetingImport(),
    onOpenRecordings: () => void openRecordingsFolder(),
    onOpenRecorder: openRecorder,
    onOpenAgent: openChat,
  });

  return (
    <MotionProvider>
      <Toaster />
      <AppEventListeners />
      <DetectionListeners />
      <AppContent
        onboardingStep={onboardingStep}
        onAccessibilityComplete={handleAccessibilityComplete}
        onModelSelected={handleModelSelected}
        direction={direction}
        currentSection={currentSection}
        onSectionChange={navigateToSection}
        onOpenMeeting={openMeeting}
        onOpenRecorder={openRecorder}
        loadingLabel={t("common.loading")}
        meetingInvalidation={meetingInvalidation}
        meetingNavigationRequest={meetingNavigationRequest}
        meetingStartRequest={meetingStartRequest}
        personRequest={personRequest}
        organizationRequest={organizationRequest}
        dictationRequest={dictationRequest}
        settingsNavigationRequest={settingsNavigationRequest}
        chatOpen={chatOpen}
        onChatOpenChange={setChatOpen}
        commandOpen={commandOpen}
        commandActions={commandActions}
        commandSeed={commandSeed}
        /* Pairing and the D14 consent decide whether the ask row is offered,
         * so the palette reads the same settings the panel's pairing screen
         * and the meeting-intelligence switch write. ⌘K's pack is verbatim
         * transcript text, so it is the same consent a meeting's summary is
         * written under. */
        agentPanel={{
          enabled: agentEnabled,
          paired: settings?.agent_panel_paired === true,
          remoteIntelligence:
            settings?.meeting_remote_intelligence_enabled === true,
        }}
        onCommandOpenChange={setPalette}
        onCommandOpen={openCommandPalette}
      />
      <RecorderDialog
        open={recorderOpen}
        onOpenChange={(nextOpen) => setModal(nextOpen ? "recorder" : "none")}
      />
      {/* The app's one import dialog. Every surface that offers an import —
       * the palette, Capture's hero, Library's toolbar and empty state, the
       * Meetings home — asks for it through `useAudioImport`/`useMeetingImport`
       * and this is where it appears. */}
      <ImportDialogHost />
    </MotionProvider>
  );
}

export default App;
