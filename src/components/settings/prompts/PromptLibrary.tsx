import React, { useCallback, useEffect, useState } from "react";
import { Plus } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/vg/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/vg/tabs";
import { SettingsPage } from "@/components/settings/rows";
import { MeetingPrompts } from "../meetings/MeetingPrompts";
import { usePromptShellStore } from "../meetings/promptTargets";
import { DictationPrompts } from "../vocabulary/PromptLibrary";

type PromptKind = "saved" | "dictation";

interface EditorRequest {
  kind: PromptKind;
  nonce: number;
}

export const PromptLibrary: React.FC = () => {
  const { t } = useTranslation();
  const [kind, setKind] = useState<PromptKind>("saved");
  const [editorRequest, setEditorRequest] = useState<EditorRequest | null>(
    null,
  );
  const newPromptRequest = usePromptShellStore(
    (state) => state.newPromptRequest,
  );
  const requestNewPrompt = usePromptShellStore(
    (state) => state.requestNewPrompt,
  );
  const consumeNewPromptRequest = usePromptShellStore(
    (state) => state.consumeNewPromptRequest,
  );

  useEffect(() => {
    if (!consumeNewPromptRequest(newPromptRequest)) return;
    setEditorRequest({ kind, nonce: newPromptRequest });
  }, [consumeNewPromptRequest, kind, newPromptRequest]);

  const finishRequest = useCallback((nonce: number) => {
    setEditorRequest((current) => (current?.nonce === nonce ? null : current));
  }, []);

  return (
    <SettingsPage
      title={t("prompts.title")}
      actions={
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={requestNewPrompt}
          data-testid="prompt-create"
        >
          <Plus aria-hidden="true" />
          {t("prompts.new")}
        </Button>
      }
      data-testid="prompt-library"
    >
      <Tabs
        value={kind}
        onValueChange={(value) => {
          if (value === "saved" || value === "dictation") setKind(value);
        }}
      >
        <TabsList variant="line" aria-label={t("prompts.title")}>
          <TabsTrigger value="saved">
            {t("settingsV2.advanced.meetings")}
          </TabsTrigger>
          <TabsTrigger value="dictation">
            {t("settingsV2.advanced.dictation")}
          </TabsTrigger>
        </TabsList>
        <TabsContent value="saved" className="pt-6">
          <MeetingPrompts
            createRequest={
              editorRequest?.kind === "saved" ? editorRequest.nonce : null
            }
            onCreateRequestHandled={finishRequest}
          />
        </TabsContent>
        <TabsContent value="dictation" className="pt-6">
          <DictationPrompts
            createRequest={
              editorRequest?.kind === "dictation" ? editorRequest.nonce : null
            }
            onCreateRequestHandled={finishRequest}
          />
        </TabsContent>
      </Tabs>
    </SettingsPage>
  );
};
