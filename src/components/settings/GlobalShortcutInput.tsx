import React, { useEffect, useState, useRef } from "react";
import { useTranslation } from "react-i18next";
import { getKeyName, normalizeKey } from "../../lib/utils/keyboard";
import { Notice, RowReset, SettingsRow } from "./rows";
import {
  ShortcutHoldHint,
  ShortcutRecorderField,
} from "./ShortcutRecorderField";
import { useSettings } from "../../hooks/useSettings";
import { useOsType } from "../../hooks/useOsType";
import { commands } from "@/bindings";
import { toast } from "sonner";

interface GlobalShortcutInputProps {
  shortcutId: string;
  disabled?: boolean;
}

export const GlobalShortcutInput: React.FC<GlobalShortcutInputProps> = ({
  shortcutId,
  disabled = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateBinding, resetBinding, isUpdating, isLoading } =
    useSettings();
  const [keyPressed, setKeyPressed] = useState<string[]>([]);
  const [recordedKeys, setRecordedKeys] = useState<string[]>([]);
  const [editingShortcutId, setEditingShortcutId] = useState<string | null>(
    null,
  );
  const [originalBinding, setOriginalBinding] = useState<string>("");
  const shortcutRefs = useRef<Map<string, HTMLDivElement | null>>(new Map());
  const osType = useOsType();

  const bindings = getSetting("bindings") || {};

  useEffect(() => {
    // Only add event listeners when we're in editing mode
    if (editingShortcutId === null) return;

    let cleanup = false;

    // Keyboard event listeners
    const handleKeyDown = async (e: KeyboardEvent) => {
      if (cleanup) return;
      if (e.repeat) return; // ignore auto-repeat
      e.preventDefault();

      // Get the key with OS-specific naming and normalize it
      const rawKey = getKeyName(e, osType);
      const key = normalizeKey(rawKey);

      if (!keyPressed.includes(key)) {
        setKeyPressed((prev) => [...prev, key]);
        // Also add to recorded keys if not already there
        if (!recordedKeys.includes(key)) {
          setRecordedKeys((prev) => [...prev, key]);
        }
      }
    };

    const handleKeyUp = async (e: KeyboardEvent) => {
      if (cleanup) return;
      e.preventDefault();

      // Get the key with OS-specific naming and normalize it
      const rawKey = getKeyName(e, osType);
      const key = normalizeKey(rawKey);

      // Remove from currently pressed keys
      setKeyPressed((prev) => prev.filter((k) => k !== key));

      // If no keys are pressed anymore, commit the shortcut
      const updatedKeyPressed = keyPressed.filter((k) => k !== key);
      if (updatedKeyPressed.length === 0 && recordedKeys.length > 0) {
        // Create the shortcut string from all recorded keys
        // Sort keys so modifiers come first, then the main key
        const modifiers = [
          "ctrl",
          "control",
          "shift",
          "alt",
          "option",
          "meta",
          "command",
          "cmd",
          "super",
          "win",
          "windows",
        ];
        const sortedKeys = [...recordedKeys].sort((a, b) => {
          const aIsModifier = modifiers.includes(a.toLowerCase());
          const bIsModifier = modifiers.includes(b.toLowerCase());
          if (aIsModifier && !bIsModifier) return -1;
          if (!aIsModifier && bIsModifier) return 1;
          return 0;
        });
        const newShortcut = sortedKeys.join("+");

        if (editingShortcutId && getSetting("bindings")?.[editingShortcutId]) {
          try {
            await updateBinding(editingShortcutId, newShortcut);
          } catch (error) {
            console.error("Failed to change binding:", error);
            toast.error(
              t("settings.general.shortcut.errors.set", {
                error: String(error),
              }),
            );

            // Reset to original binding on error
            if (originalBinding) {
              try {
                await updateBinding(editingShortcutId, originalBinding);
              } catch (resetError) {
                console.error("Failed to reset binding:", resetError);
                toast.error(t("settings.general.shortcut.errors.reset"));
              }
            }
          }

          // Re-register all bindings (the one just committed is already
          // registered; re-registering it fails cleanly and is ignored)
          await commands.resumeAllBindings().catch(console.error);

          // Exit editing mode and reset states
          setEditingShortcutId(null);
          setKeyPressed([]);
          setRecordedKeys([]);
          setOriginalBinding("");
        }
      }
    };

    // Add click outside handler
    const handleClickOutside = async (e: MouseEvent) => {
      if (cleanup) return;
      const activeElement = shortcutRefs.current.get(editingShortcutId);
      const target = e.target;
      if (
        activeElement &&
        target instanceof Node &&
        !activeElement.contains(target)
      ) {
        // Cancel shortcut recording and restore original binding
        if (editingShortcutId && originalBinding) {
          try {
            await updateBinding(editingShortcutId, originalBinding);
          } catch (error) {
            console.error("Failed to restore original binding:", error);
            toast.error(t("settings.general.shortcut.errors.restore"));
          }
        }
        await commands.resumeAllBindings().catch(console.error);
        setEditingShortcutId(null);
        setKeyPressed([]);
        setRecordedKeys([]);
        setOriginalBinding("");
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("keyup", handleKeyUp);
    window.addEventListener("click", handleClickOutside);

    return () => {
      cleanup = true;
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("keyup", handleKeyUp);
      window.removeEventListener("click", handleClickOutside);
    };
  }, [
    keyPressed,
    recordedKeys,
    editingShortcutId,
    getSetting,
    originalBinding,
    updateBinding,
    osType,
    t,
  ]);

  // Start recording a new shortcut
  const startRecording = async (id: string) => {
    if (editingShortcutId === id) return; // Already editing this shortcut

    // Suspend all bindings so no shortcut fires (or swallows the
    // keystrokes) while keys are being recorded
    await commands.suspendAllBindings().catch(console.error);

    // Store the original binding to restore if canceled
    setOriginalBinding(bindings[id]?.current_binding || "");
    setEditingShortcutId(id);
    setKeyPressed([]);
    setRecordedKeys([]);
  };

  // Store references to shortcut elements
  const setShortcutRef = (id: string, ref: HTMLDivElement | null) => {
    shortcutRefs.current.set(id, ref);
  };

  // If still loading, show loading state
  if (isLoading) {
    return (
      <SettingsRow label={t("settings.general.shortcut.title")}>
        <Notice>{t("settings.general.shortcut.loading")}</Notice>
      </SettingsRow>
    );
  }

  // If no bindings are loaded, show empty state
  if (Object.keys(bindings).length === 0) {
    return (
      <SettingsRow label={t("settings.general.shortcut.title")}>
        <Notice>{t("settings.general.shortcut.none")}</Notice>
      </SettingsRow>
    );
  }

  const binding = bindings[shortcutId];
  if (!binding) {
    /* The row still names WHICH shortcut is missing: a generic "Shortcuts"
     * title under the Shortcuts section header was the same word twice with
     * an unexplained error under it. */
    return (
      <SettingsRow
        label={t(
          `settings.general.shortcut.bindings.${shortcutId}.name`,
          t("settings.general.shortcut.title"),
        )}
      >
        <Notice tone="danger">{t("settings.general.shortcut.notFound")}</Notice>
      </SettingsRow>
    );
  }

  const translatedName = t(
    `settings.general.shortcut.bindings.${shortcutId}.name`,
    binding.name,
  );

  /* The chord is the row's answer, so the row says its name once and nothing
   * else. Tap/hold is the one thing the caps cannot show, and it rides on the
   * dictation binding only — repeating it on cancel and command was the same
   * sentence three times down one section. */
  return (
    <SettingsRow
      label={translatedName}
      hint={shortcutId === "transcribe" ? <ShortcutHoldHint /> : undefined}
      disabled={disabled}
    >
      <ShortcutRecorderField
        chord={binding.current_binding}
        recording={editingShortcutId === shortcutId}
        captured={recordedKeys.join("+")}
        onStartRecording={() => startRecording(shortcutId)}
        disabled={disabled}
        recordingRef={(node) => setShortcutRef(shortcutId, node)}
        bindingName={translatedName}
      />
      <RowReset
        name={translatedName}
        changed={binding.current_binding !== binding.default_binding}
        disabled={disabled || isUpdating(`binding_${shortcutId}`)}
        onReset={() => resetBinding(shortcutId)}
      />
    </SettingsRow>
  );
};
