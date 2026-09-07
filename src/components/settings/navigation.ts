export type SettingsNavigationTarget =
  | { tab: "essentials" }
  | { tab: "advanced"; section?: "meetings" | "sonaAgent" }
  | { tab: "prompts" }
  | { tab: "debug" };

export interface SettingsNavigationRequest {
  target: SettingsNavigationTarget;
  nonce: number;
}

export const DEFAULT_SETTINGS_TARGET = {
  tab: "essentials",
} as const satisfies SettingsNavigationTarget;

export const nextSettingsNavigationRequest = (
  current: SettingsNavigationRequest | null,
  target: SettingsNavigationTarget,
): SettingsNavigationRequest => ({
  target,
  nonce: (current?.nonce ?? 0) + 1,
});
