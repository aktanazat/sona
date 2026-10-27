import React, { useId, useMemo, useState } from "react";
import { ExternalLink, ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type PostProcessProvider,
  type PostProcessProviderConsent,
} from "@/bindings";
import { Notice, SettingsField } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Checkbox } from "@/components/vg/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { Label } from "@/components/vg/label";

interface RemoteProviderConsentProps {
  provider: PostProcessProvider | undefined;
  consent: PostProcessProviderConsent | undefined;
  endpointChanged: boolean;
  onAccepted: () => Promise<void>;
}

type EndpointState =
  | { kind: "local" }
  | { kind: "invalid" }
  | { kind: "remote"; endpoint: string };

const endpointStateForProvider = (
  provider: PostProcessProvider,
): EndpointState => {
  try {
    const endpoint = new URL(provider.base_url.trim());
    const isLoopback =
      endpoint.hostname === "localhost" ||
      endpoint.hostname === "127.0.0.1" ||
      endpoint.hostname === "::1";

    if (
      provider.id === "apple_intelligence" ||
      provider.id === "sona_local" ||
      isLoopback
    ) {
      return { kind: "local" };
    }

    if (endpoint.protocol !== "https:") {
      return { kind: "invalid" };
    }

    endpoint.username = "";
    endpoint.password = "";
    endpoint.search = "";
    endpoint.hash = "";
    endpoint.pathname = endpoint.pathname.replace(/\/+$/, "");

    return {
      kind: "remote",
      endpoint: endpoint.toString().replace(/\/$/, ""),
    };
  } catch {
    return { kind: "invalid" };
  }
};

interface RemoteProviderConsentContentProps {
  provider: PostProcessProvider;
  consent: PostProcessProviderConsent | undefined;
  endpoint: string;
  endpointChanged: boolean;
  onAccepted: () => Promise<void>;
}

const RemoteProviderConsentContent: React.FC<
  RemoteProviderConsentContentProps
> = ({ provider, consent, endpoint, endpointChanged, onAccepted }) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);
  const [saving, setSaving] = useState(false);
  const [errorKey, setErrorKey] = useState<string | null>(null);
  const acknowledgeId = useId();
  const currentConsent =
    consent?.endpoint === endpoint && consent.text_transfer_consent;

  const accept = async () => {
    if (!acknowledged || saving) {
      return;
    }

    setSaving(true);
    setErrorKey(null);
    try {
      const result = await commands.acceptPostProcessProviderConsent(
        provider.id,
      );
      if (result.status === "error") {
        if (result.error === "invalid_destination") {
          setErrorKey(
            "settings.postProcessing.remoteConsent.invalidDestination",
          );
        } else {
          setErrorKey("settings.postProcessing.remoteConsent.saveFailed");
        }
        return;
      }

      await onAccepted();
      setOpen(false);
    } catch {
      setErrorKey("settings.postProcessing.remoteConsent.saveFailed");
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <SettingsField label={t("settings.postProcessing.remoteConsent.title")}>
        {/* The one sentence in this block that is not a restatement: which
         * endpoint text can reach, and whether it already may. That is the
         * consent itself, so it stays on the surface rather than behind an
         * info affordance. */}
        <div className="flex flex-wrap items-center justify-between gap-3">
          <Notice live={false} className="min-w-0 flex-1">
            {currentConsent
              ? t("settings.postProcessing.remoteConsent.current", { endpoint })
              : t("settings.postProcessing.remoteConsent.description", {
                  provider: provider.label,
                  endpoint,
                })}
          </Notice>
          <Button
            type="button"
            variant={currentConsent ? "outline" : "default"}
            size="sm"
            onClick={() => setOpen(true)}
          >
            <ShieldCheck aria-hidden="true" />
            {currentConsent
              ? t("settings.postProcessing.remoteConsent.review")
              : t("settings.postProcessing.remoteConsent.acknowledge")}
          </Button>
        </div>
      </SettingsField>
      {endpointChanged && !currentConsent ? (
        <div className="px-6 py-3">
          <Notice tone="warning">
            {t("settings.postProcessing.remoteConsent.endpointChanged")}
          </Notice>
        </div>
      ) : null}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {t("settings.postProcessing.remoteConsent.title")}
            </DialogTitle>
            <DialogDescription>
              {t("settings.postProcessing.remoteConsent.description", {
                provider: provider.label,
                endpoint,
              })}
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-3">
            <p className="text-sm text-gray-800">
              {t("settings.postProcessing.remoteConsent.endpointLabel")}
            </p>
            <code className="block rounded-md border border-gray-alpha-400 bg-background-200 px-3 py-2 text-xs break-all text-gray-1000">
              {endpoint}
            </code>
            <div className="flex items-start gap-2">
              <Checkbox
                id={acknowledgeId}
                checked={acknowledged}
                onCheckedChange={(checked) => setAcknowledged(checked === true)}
                disabled={saving || currentConsent}
              />
              <Label
                htmlFor={acknowledgeId}
                className="leading-5 font-normal text-gray-900"
              >
                {t("settings.postProcessing.remoteConsent.acknowledge")}
              </Label>
            </div>
            {errorKey ? <Notice tone="danger">{t(errorKey)}</Notice> : null}
            <p className="flex items-center gap-1 text-xs text-gray-800">
              <ExternalLink aria-hidden="true" className="size-3" />
              {t("settings.postProcessing.remoteConsent.destinationPinned")}
            </p>
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setOpen(false)}
              disabled={saving}
            >
              {t("settings.postProcessing.remoteConsent.cancel")}
            </Button>
            <Button
              type="button"
              size="sm"
              onClick={() => void accept()}
              disabled={!acknowledged || saving || currentConsent}
            >
              {saving
                ? t("common.saving")
                : t("settings.postProcessing.remoteConsent.accept")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
};

export const RemoteProviderConsent: React.FC<RemoteProviderConsentProps> = ({
  provider,
  consent,
  endpointChanged,
  onAccepted,
}) => {
  const { t } = useTranslation();
  const endpointState = useMemo(
    () => (provider ? endpointStateForProvider(provider) : null),
    [provider],
  );

  if (!provider || endpointState === null || endpointState.kind === "local") {
    return null;
  }

  if (endpointState.kind === "invalid") {
    return (
      <div className="px-6 py-3">
        <Notice tone="danger">
          {t("settings.postProcessing.remoteConsent.invalidDestination")}
        </Notice>
      </div>
    );
  }

  return (
    <RemoteProviderConsentContent
      key={provider.id + ":" + endpointState.endpoint}
      provider={provider}
      consent={consent}
      endpoint={endpointState.endpoint}
      endpointChanged={endpointChanged}
      onAccepted={onAccepted}
    />
  );
};
