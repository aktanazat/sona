import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  events,
  type AgentBridgeSettings,
  type AgentBridgeStatus,
} from "@/bindings";
import {
  EMPTY_BRIDGE_SETTINGS,
  agentBridgeViewReducer,
} from "./agentBridgeView";

const subscribeToAgentBridgeUpdates = (
  onUpdate: (status: AgentBridgeStatus) => void,
  onError: (message: string) => void,
) => {
  let disposed = false;
  let unlisten: (() => void) | undefined;

  void events.agentBridgeUpdateEvent
    .listen((event) => {
      if (!disposed) onUpdate(event.payload.status);
    })
    .then(
      (nextUnlisten) => {
        if (disposed) {
          nextUnlisten();
        } else {
          unlisten = nextUnlisten;
        }
      },
      (error) =>
        onError(error instanceof Error ? error.message : String(error)),
    );

  return () => {
    disposed = true;
    unlisten?.();
  };
};

/* The read side of the agent bridge: one refresh reads status, sessions,
 * requests and the pending queue together, and the backend's update event
 * replays that same read, so a change the agent makes on its own arrives
 * without the page asking again. */
export const useAgentBridgeObservations = (
  bridgeSettings: AgentBridgeSettings | undefined,
  active = true,
) => {
  const { t, i18n } = useTranslation();
  const expiryTimeFormatter = useMemo(
    () =>
      new Intl.DateTimeFormat(i18n.language, {
        hour: "numeric",
        minute: "2-digit",
        second: "2-digit",
      }),
    [i18n.language],
  );

  const [view, updateView] = useReducer(agentBridgeViewReducer, {
    bridge: bridgeSettings ?? EMPTY_BRIDGE_SETTINGS,
    status: null,
    sessions: [],
    requests: [],
    pendingMessages: [],
    hookSnippet: null,
    hookError: null,
    error: null,
    loading: true,
    authorizing: false,
    replySessionId: "",
    replyText: "",
  });
  const { sessions, replySessionId } = view;
  const mountedRef = useRef(true);
  const refreshObservationsRef = useRef<() => Promise<void>>(
    async () => undefined,
  );

  const refreshObservations = useCallback(async () => {
    if (!active) return;
    updateView({ loading: true });
    try {
      const [nextStatus, nextSessions, nextRequests, nextPendingMessages] =
        await Promise.all([
          commands.getAgentBridgeStatus(),
          commands.getAgentBridgeSessions(),
          commands.getAgentBridgeRequests(),
          commands.getAgentBridgePendingMessages(),
        ]);
      if (!mountedRef.current) return;
      updateView({
        status: nextStatus,
        sessions: nextSessions,
        requests: nextRequests,
        pendingMessages: nextPendingMessages,
        error: null,
      });
    } catch (refreshError) {
      if (mountedRef.current) {
        updateView({
          error: t("settings.agents.errors.load", {
            error: String(refreshError),
          }),
        });
      }
    } finally {
      if (mountedRef.current) updateView({ loading: false });
    }
  }, [active, t]);

  useEffect(() => {
    refreshObservationsRef.current = refreshObservations;
  }, [refreshObservations]);

  useEffect(() => {
    if (!active) {
      mountedRef.current = false;
      updateView({ loading: false });
      return;
    }
    mountedRef.current = true;
    void refreshObservations();
    return () => {
      mountedRef.current = false;
    };
  }, [active, refreshObservations]);

  useEffect(() => {
    if (!active) return;
    const unsubscribe = subscribeToAgentBridgeUpdates(
      (nextStatus) => {
        updateView({ status: nextStatus });
        void refreshObservationsRef.current();
      },
      (subscriptionError) => {
        console.error("Agent bridge subscription failed:", subscriptionError);
      },
    );

    return () => {
      unsubscribe();
    };
  }, [active]);

  useEffect(() => {
    if (!active) return;
    let disposed = false;
    void commands
      .getAgentBridgeHookSnippet()
      .then((result) => {
        if (disposed) return;
        if (result.status === "ok") {
          updateView({ hookSnippet: result.data, hookError: null });
        } else {
          updateView({ hookError: String(result.error) });
        }
      })
      .catch((snippetError) => {
        if (!disposed) updateView({ hookError: String(snippetError) });
      });
    return () => {
      disposed = true;
    };
  }, [active]);

  useEffect(() => {
    if (bridgeSettings) updateView({ bridge: bridgeSettings });
  }, [bridgeSettings]);

  const replySessions = useMemo(
    () =>
      sessions.filter(
        (session) => session.agent === "claude" || session.agent === "omp",
      ),
    [sessions],
  );

  useEffect(() => {
    if (!replySessions.some((session) => session.id === replySessionId)) {
      updateView({ replySessionId: replySessions[0]?.id ?? "" });
    }
  }, [replySessions, replySessionId]);

  return {
    view,
    updateView,
    replySessions,
    expiryTimeFormatter,
    refreshObservations,
  };
};
