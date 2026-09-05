import React, { useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ChevronDown } from "lucide-react";
import { Button } from "@/components/vg/button";
import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/vg/command";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/vg/popover";
import { FIELD_MAX_W, RowReset, SettingsRow } from "./rows";
import { useSettings } from "../../hooks/useSettings";
import {
  getLanguageLabel,
  recognitionLanguage,
  SELECTABLE_LANGUAGES,
  supportsLanguageCode,
} from "../../lib/constants/languages";

interface LanguageSelectorProps {
  supportedLanguages?: string[];
  // Whether the model can auto-detect language. Gates the "Auto" option:
  // must-pick models (no detection) omit it and force a concrete choice.
  supportsLanguageDetection?: boolean;
}

// Mirrors the matching logic of `effective_language` in
// src-tauri/src/managers/model.rs. The Rust function is authoritative for the
// *concrete* code the engine receives (e.g. "en-US"); this resolves the
// canonical *base* code ("en") so the highlighted picker item matches an entry
// in the LANGUAGES list. Matching is base-aware (`supportsLanguageCode` strips
// region/script subtags), so a model advertising full locales still resolves.
const effectiveLanguage = (
  intent: string,
  supported: string[],
  supportsDetection: boolean,
): string => {
  if (supported.length === 0) return intent;
  if (intent !== "auto" && supportsLanguageCode(supported, intent))
    return intent;
  if (supportsDetection) return "auto";
  if (supportsLanguageCode(supported, "en")) return "en";
  return recognitionLanguage(supported[0]);
};

/**
 * The recognition language, as a searchable list of a hundred-odd names.
 *
 * The list is a Command inside a Popover rather than a Select: with this many
 * items the search field is the control, and cmdk owns filtering, first-match
 * Enter, arrow keys and Escape — all of which this file used to hand-roll.
 */
export const LanguageSelector: React.FC<LanguageSelectorProps> = ({
  supportedLanguages,
  supportsLanguageDetection = true,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, resetSetting, isUpdating } = useSettings();
  const [open, setOpen] = useState(false);
  const id = useId();

  // The persisted *intent* (auto | code). What's actually used/shown is the
  // effective value resolved against the current model's capabilities.
  const intent = getSetting("selected_language") || "auto";
  const selectedLanguage = effectiveLanguage(
    intent,
    supportedLanguages ?? [],
    supportsLanguageDetection,
  );

  const availableLanguages = useMemo(() => {
    if (!supportedLanguages || supportedLanguages.length === 0)
      return SELECTABLE_LANGUAGES;
    return SELECTABLE_LANGUAGES.filter((lang) =>
      lang.value === "auto"
        ? supportsLanguageDetection
        : supportsLanguageCode(supportedLanguages, lang.value),
    );
  }, [supportedLanguages, supportsLanguageDetection]);

  const selectedLanguageName =
    getLanguageLabel(selectedLanguage) || t("settings.general.language.auto");
  const busy = isUpdating("selected_language");
  const label = t("settings.general.language.title");

  const handleLanguageSelect = async (languageCode: string) => {
    setOpen(false);
    await updateSetting("selected_language", languageCode);
  };

  return (
    <SettingsRow label={label} controlId={id}>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            id={id}
            variant="outline"
            size="sm"
            role="combobox"
            aria-expanded={open}
            disabled={busy}
            className={`w-auto justify-between font-normal ${FIELD_MAX_W}`}
          >
            <span className="truncate">{selectedLanguageName}</span>
            <ChevronDown aria-hidden="true" className="opacity-50" />
          </Button>
        </PopoverTrigger>
        <PopoverContent align="end" className="w-60 p-0">
          <Command>
            {/* cmdk filters on each item's `value`, so the value is the name a
                reader types — the code goes to the select handler instead. */}
            <CommandInput
              placeholder={t("settings.general.language.searchPlaceholder")}
            />
            <CommandList>
              <CommandEmpty>
                {t("settings.general.language.noResults")}
              </CommandEmpty>
              {availableLanguages.map((language) => (
                <CommandItem
                  key={language.value}
                  value={language.label}
                  onSelect={() => void handleLanguageSelect(language.value)}
                >
                  <Check
                    aria-hidden="true"
                    className={
                      selectedLanguage === language.value
                        ? "opacity-100"
                        : "opacity-0"
                    }
                  />
                  <span className="truncate">{language.label}</span>
                </CommandItem>
              ))}
            </CommandList>
          </Command>
        </PopoverContent>
      </Popover>
      <RowReset
        name={label}
        /* The persisted intent, not the language the model resolved it to:
         * "auto" IS the default, however concrete a must-pick model renders
         * it. */
        changed={intent !== "auto"}
        disabled={busy}
        onReset={() => void resetSetting("selected_language")}
      />
    </SettingsRow>
  );
};
