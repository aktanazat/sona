import React, { useId } from "react";
import { useTranslation } from "react-i18next";
import type { AudioDevice } from "@/bindings";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { FIELD_MAX_W, RowReset, SettingsRow } from "./rows";
import { useSettings } from "../../hooks/useSettings";

/* The settings whose value is the NAME of an audio device. Closed on purpose:
 * these three are the ones the sentinel below is true of. */
type DeviceSettingKey =
  | "selected_microphone"
  | "clamshell_microphone"
  | "selected_output_device";

/* Every enumeration is prepended with a device literally named "Default"
 * (src-tauri/src/commands/audio.rs:198,256) while the setting persists the
 * lowercase sentinel `"default"` for it. The row has to spell the value the
 * enumerated way or Radix matches no item. It is also the device in effect
 * when the setting is unset, which is why this row has no placeholder: there
 * is no state in which nothing is selected. */
const DEFAULT_DEVICE = "Default";

/**
 * A settings row that names one audio device and lets the user pick another.
 *
 * The trigger is the only place the row reports which device is in effect, and
 * it is handed a persisted name against a list enumerated at render time — so
 * the two disagreeing is a routine state, not an error path: a mic gets
 * unplugged, an enumeration has not resolved yet. In all of those states the
 * row keeps NAMING the configured device, because falling back to a
 * placeholder would say nothing is selected, which is a lie about state.
 * Radix portals a label into the trigger only out of a mounted, selected
 * `SelectItem`, so the name has to be given to `SelectValue` as children.
 *
 * It deliberately stops there and does not call the device unavailable: this
 * row sees a list, so it cannot tell an enumeration that confirmed a device
 * gone from one that failed, was denied, or is still in flight. Only the owner
 * of the enumeration knows that — which is why `devices` and `refresh` are
 * passed in rather than chosen here.
 */
export const DeviceSelectRow: React.FC<{
  settingKey: DeviceSettingKey;
  labelKey: string;
  /** The enumeration to pick from, as fresh as its last read. */
  devices: AudioDevice[];
  /** Re-read that enumeration. Called on the way open. */
  refresh: () => Promise<void>;
  /** The one thing about the row a reader cannot infer. Usually absent. */
  hintKey?: string;
  disabled?: boolean;
}> = ({
  settingKey,
  labelKey,
  devices,
  refresh,
  hintKey,
  disabled = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, resetSetting, isUpdating, isLoading } =
    useSettings();
  const id = useId();

  const saved = getSetting(settingKey);
  /* A device's label IS its name, so the option-list lookup a generic control
   * would do here resolves to the saved value itself. Total by construction:
   * unset and the sentinel both read as the default device. */
  const device = saved === "default" ? DEFAULT_DEVICE : saved || DEFAULT_DEVICE;

  const busy = isUpdating(settingKey);
  const label = t(labelKey);

  return (
    <SettingsRow
      label={label}
      hint={hintKey === undefined ? undefined : t(hintKey)}
      controlId={id}
      disabled={disabled}
    >
      <Select
        value={device}
        onValueChange={(deviceName) =>
          void updateSetting(settingKey, deviceName)
        }
        /* The device list is only as fresh as the last enumeration, so it is
         * re-read on the way open — the same moment the old dropdown used. */
        onOpenChange={(open) => {
          if (open) void refresh();
        }}
        disabled={disabled || busy || isLoading || devices.length === 0}
      >
        <SelectTrigger id={id} size="sm" className={`w-auto ${FIELD_MAX_W}`}>
          <SelectValue>{device}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {devices.map((available) => (
            <SelectItem key={available.name} value={available.name}>
              {available.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <RowReset
        name={label}
        /* Unset and the sentinel both mean the default device, and `device`
         * has already resolved both to its name. */
        changed={device !== DEFAULT_DEVICE}
        disabled={disabled || busy || isLoading}
        onReset={() => void resetSetting(settingKey)}
      />
    </SettingsRow>
  );
};
