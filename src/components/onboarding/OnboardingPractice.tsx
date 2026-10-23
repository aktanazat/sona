import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { commands } from "@/bindings";
import { Button } from "@/components/vg/button";
import { Textarea } from "@/components/vg/textarea";

type Phase =
  | "ready"
  | "starting"
  | "recording"
  | "transcribing"
  | "delivered"
  | "finishing"
  | "cancelling"
  | "error";

interface PracticeExpired {
  session_id: string;
  error: string;
}

interface OnboardingPracticeProps {
  onComplete: () => void;
  onBack: () => void;
}

const RECORDING_SECONDS = 30;

export default function OnboardingPractice({
  onComplete,
  onBack,
}: OnboardingPracticeProps) {
  const { t } = useTranslation();
  const [phase, setPhase] = useState<Phase>("ready");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [remaining, setRemaining] = useState(RECORDING_SECONDS);
  const phaseRef = useRef<Phase>("ready");
  const session = useRef<string | null>(null);
  const operation = useRef(0);
  const mounted = useRef(true);
  const unlisten = useRef<UnlistenFn | null>(null);
  const deadline = useRef(0);

  const transition = useCallback((next: Phase) => {
    phaseRef.current = next;
    setPhase(next);
  }, []);

  const isCurrent = useCallback(
    (id: string, request: number) =>
      mounted.current &&
      session.current === id &&
      operation.current === request,
    [],
  );

  const errorMessage = useCallback(
    (failure: string): string => {
      const message = failure.toLowerCase();
      if (message.includes("permission") || message.includes("denied")) {
        return t("onboarding.practice.errors.permission");
      }
      if (
        message.includes("no_speech") ||
        message.includes("no speech") ||
        message.includes("no words")
      ) {
        return t("onboarding.practice.errors.noSpeech");
      }
      if (
        message.includes("no_input") ||
        message.includes("no microphone found")
      ) {
        return t("onboarding.practice.errors.microphone");
      }
      if (message.includes("expired") || message.includes("timed out")) {
        return t("onboarding.practice.errors.expired");
      }
      if (message.includes("changed") || message.includes("incompatible")) {
        return t("onboarding.practice.errors.changed");
      }
      return t("onboarding.practice.errors.record", { error: failure });
    },
    [t],
  );

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      operation.current += 1;
      unlisten.current?.();
      unlisten.current = null;
      const id = session.current;
      session.current = null;
      if (id) {
        void (async () => {
          try {
            await commands.cancelOnboardingPractice(id);
          } catch (failure) {
            console.warn("Could not cancel onboarding practice:", failure);
          }
        })();
      }
    };
  }, []);

  const record = async () => {
    if (!["ready", "error", "delivered"].includes(phaseRef.current)) return;
    const request = ++operation.current;
    transition("starting");
    setError(null);
    setText("");
    setRemaining(RECORDING_SECONDS);
    try {
      const previous = session.current;
      if (previous) {
        const cancelled = await commands.cancelOnboardingPractice(previous);
        if (!isCurrent(previous, request)) return;
        if (cancelled.status === "error") throw cancelled.error;
      }
      if (!mounted.current || operation.current !== request) return;
      const id = crypto.randomUUID();
      session.current = id;
      if (!unlisten.current) {
        const cleanup = await listen<PracticeExpired>(
          "onboarding-practice-expired",
          ({ payload }) => {
            if (!mounted.current || session.current !== payload.session_id)
              return;
            operation.current += 1;
            session.current = null;
            setText("");
            setError(errorMessage(payload.error));
            transition("error");
          },
        );
        if (!isCurrent(id, request)) {
          cleanup();
          return;
        }
        unlisten.current = cleanup;
      }
      const result = await commands.startOnboardingPractice(id);
      if (!isCurrent(id, request)) return;
      if (result.status === "error") throw result.error;
      deadline.current = performance.now() + RECORDING_SECONDS * 1000;
      transition("recording");
    } catch (failure) {
      if (!mounted.current || operation.current !== request) return;
      setError(errorMessage(String(failure)));
      transition("error");
    }
  };

  const stop = useCallback(async () => {
    const id = session.current;
    if (!id || phaseRef.current !== "recording") return;
    const request = ++operation.current;
    transition("transcribing");
    try {
      const result = await commands.stopOnboardingPractice(id);
      if (!isCurrent(id, request)) return;
      if (result.status === "error") throw result.error;
      if (result.data.session_id !== id || !result.data.text.trim()) {
        throw new Error("no_speech");
      }
      setText(result.data.text);
      transition("delivered");
    } catch (failure) {
      if (!isCurrent(id, request)) return;
      setError(errorMessage(String(failure)));
      transition("error");
    }
  }, [errorMessage, isCurrent, transition]);

  useEffect(() => {
    if (phase !== "recording") return;
    const tick = () => {
      const seconds = Math.max(
        0,
        Math.ceil((deadline.current - performance.now()) / 1000),
      );
      setRemaining(Math.min(RECORDING_SECONDS, seconds));
      if (seconds === 0) void stop();
    };
    const timer = window.setInterval(tick, 250);
    return () => window.clearInterval(timer);
  }, [phase, stop]);

  const cancel = async (back = false) => {
    if (phaseRef.current === "cancelling" || phaseRef.current === "finishing")
      return;
    const id = session.current;
    const request = ++operation.current;
    transition("cancelling");
    try {
      if (id) {
        const result = await commands.cancelOnboardingPractice(id);
        if (!isCurrent(id, request)) return;
        if (result.status === "error") throw result.error;
      }
      if (!mounted.current || operation.current !== request) return;
      session.current = null;
      setText("");
      setError(null);
      transition("ready");
      if (back) onBack();
    } catch {
      if (!mounted.current || operation.current !== request) return;
      setError(t("onboarding.practice.errors.cancel"));
      transition("error");
    }
  };

  const finish = async () => {
    const id = session.current;
    if (!id || phaseRef.current !== "delivered" || !text.trim()) return;
    const request = ++operation.current;
    transition("finishing");
    setError(null);
    try {
      const result = await commands.completeOnboardingPractice(id);
      if (!isCurrent(id, request)) return;
      if (result.status === "error") throw result.error;
      session.current = null;
      onComplete();
    } catch (failure) {
      if (!isCurrent(id, request)) return;
      setError(
        t("onboarding.practice.errors.complete", { error: String(failure) }),
      );
      transition("delivered");
    }
  };

  const capturing =
    phase === "starting" || phase === "recording" || phase === "transcribing";
  const busy = capturing || phase === "cancelling" || phase === "finishing";
  const delivered = phase === "delivered" || phase === "finishing";

  return (
    <>
      <h1 className="ob-headline">{t("onboarding.practice.title")}</h1>
      <div className="ob-practice">
        <p className="ob-practice-status" role="status">
          {phase === "recording"
            ? t("onboarding.practice.recording", { seconds: remaining })
            : t(`onboarding.practice.status.${phase}`)}
        </p>
        {delivered && (
          <div className="ob-transcript">
            <label htmlFor="onboarding-practice-text">
              {t("onboarding.practice.words")}
            </label>
            <Textarea
              id="onboarding-practice-text"
              value={text}
              onChange={(event) => setText(event.target.value)}
              readOnly={phase === "finishing"}
              rows={4}
            />
          </div>
        )}
        {error && (
          <p className="ob-error" role="alert">
            {error}
          </p>
        )}
        <div className="ob-actions">
          {phase === "recording" ? (
            <Button onClick={() => void stop()}>
              {t("onboarding.practice.stop")}
            </Button>
          ) : delivered ? (
            <Button
              disabled={busy || !text.trim()}
              onClick={() => void finish()}
            >
              {t("onboarding.practice.finish")}
            </Button>
          ) : (
            <Button disabled={busy} onClick={() => void record()}>
              {t(
                phase === "error"
                  ? "common.retry"
                  : "onboarding.practice.record",
              )}
            </Button>
          )}
          {delivered && (
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => void record()}
            >
              {t("onboarding.practice.recordAgain")}
            </Button>
          )}
          {(capturing || delivered) && (
            <Button
              variant="ghost"
              disabled={phase === "finishing"}
              onClick={() => void cancel()}
            >
              {t("common.cancel")}
            </Button>
          )}
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() => void cancel(true)}
          >
            {t("onboarding.back")}
          </Button>
        </div>
      </div>
    </>
  );
}
