import React, { useEffect, useId, useState } from "react";
import { Pencil, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { commands, type LLMPrompt, type Result } from "@/bindings";
import { Badge } from "@/components/vg/badge";
import { Button } from "@/components/vg/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { Input } from "@/components/vg/input";
import { Label } from "@/components/vg/label";
import { Textarea } from "@/components/vg/textarea";
import {
  Notice,
  RowActions,
  SettingsSection,
} from "@/components/settings/rows";
import { useSettings } from "../../../hooks/useSettings";
import {
  EmptyLine,
  Hint,
  literalText,
  LoadingRows,
  RuleList,
  RuleRow,
} from "./PanelParts";

/** The prompt being written. An empty id means it does not exist yet. */
interface PromptDraft {
  id: string;
  name: string;
  prompt: string;
}

const EMPTY_PROMPTS: LLMPrompt[] = [];

/**
 * Management surface for the post-processing prompts: the rewrite instructions
 * Sona sends to the LLM after transcription. The selected prompt is the one a
 * mode without its own prompt starts from.
 */
interface DictationPromptsProps {
  createRequest: number | null;
  onCreateRequestHandled: (nonce: number) => void;
}

export const DictationPrompts: React.FC<DictationPromptsProps> = ({
  createRequest,
  onCreateRequestHandled,
}) => {
  const { t } = useTranslation();
  const { settings, isLoading, refreshSettings } = useSettings();
  const nameFieldId = useId();
  const bodyFieldId = useId();
  const outputTipId = useId();
  const [draft, setDraft] = useState<PromptDraft | null>(null);
  const [pendingDelete, setPendingDelete] = useState<LLMPrompt | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draftError, setDraftError] = useState<string | null>(null);
  useEffect(() => {
    if (createRequest === null) return;
    setDraftError(null);
    setDraft({ id: "", name: "", prompt: "" });
    onCreateRequestHandled(createRequest);
  }, [createRequest, onCreateRequestHandled]);

  const prompts = settings?.post_process_prompts ?? EMPTY_PROMPTS;
  const selectedId = settings?.post_process_selected_prompt_id ?? null;
  const isLastPrompt = prompts.length <= 1;
  const sectionLabel = t(
    "settings.postProcessing.prompts.libraryTitle",
    "Post-processing prompts",
  );

  /* Command results carry a user-facing sentence on failure; a thrown error is
   * a transport fault. Both belong on screen, never only in the console. */
  const runCommand = async <T,>(
    command: () => Promise<Result<T, string>>,
    reportError: (message: string | null) => void,
    onSuccess?: () => void,
  ) => {
    setBusy(true);
    reportError(null);
    try {
      const result = await command();
      if (result.status !== "ok") {
        reportError(String(result.error));
        return;
      }
      await refreshSettings();
      onSuccess?.();
    } catch (thrown) {
      reportError(String(thrown));
    } finally {
      setBusy(false);
    }
  };

  const trimmedName = draft?.name.trim() ?? "";
  const trimmedBody = draft?.prompt.trim() ?? "";
  const draftIncomplete = trimmedName === "" || trimmedBody === "";

  const saveDraft = () => {
    if (!draft || draftIncomplete || busy) return;
    void runCommand(
      () =>
        draft.id === ""
          ? commands.addPostProcessPrompt(trimmedName, trimmedBody)
          : commands.updatePostProcessPrompt(
              draft.id,
              trimmedName,
              trimmedBody,
            ),
      setDraftError,
      () => setDraft(null),
    );
  };

  const library = () => {
    if (isLoading) {
      return (
        <LoadingRows
          label={t(
            "settings.postProcessing.prompts.loading",
            "Loading prompts",
          )}
          rows={2}
        />
      );
    }

    if (prompts.length === 0) {
      return (
        <EmptyLine
          text={t(
            "settings.postProcessing.prompts.empty.description",
            "A prompt tells the model what to do with the transcript, for example: rewrite the following as a short message, keeping every fact: ${output}",
          )}
        />
      );
    }

    return (
      <RuleList label={sectionLabel}>
        {prompts.map((prompt) => {
          const selected = prompt.id === selectedId;

          return (
            <RuleRow
              key={prompt.id}
              className="flex items-start gap-3"
              data-testid="prompt-row"
              data-prompt-id={prompt.id}
            >
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-2">
                  <span className="truncate text-[14px] text-gray-1000">
                    {prompt.name}
                  </span>
                  {selected && (
                    /* The prompt modes start from carries the inverted current
                     * chip. */
                    <Badge className="flex-none">
                      {t("settings.postProcessing.prompts.inUse", "In use")}
                    </Badge>
                  )}
                </span>
                <span className="mt-0.5 block line-clamp-2 text-[12.5px] leading-[18px] text-gray-700">
                  {prompt.prompt}
                </span>
              </span>
              <span className="flex flex-none items-center gap-1.5">
                {!selected && (
                  <Button
                    size="xs"
                    variant="outline"
                    disabled={busy}
                    onClick={() =>
                      void runCommand(
                        () => commands.setPostProcessSelectedPrompt(prompt.id),
                        setError,
                      )
                    }
                    aria-label={t("settings.postProcessing.prompts.useNamed", {
                      defaultValue: "Use {{name}}",
                      name: prompt.name,
                    })}
                    data-testid="prompt-use"
                  >
                    {t("settings.postProcessing.prompts.use", "Use")}
                  </Button>
                )}
                <RowActions>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-gray-700 hover:text-gray-1000"
                    disabled={busy}
                    onClick={() => {
                      setDraftError(null);
                      setDraft({
                        id: prompt.id,
                        name: prompt.name,
                        prompt: prompt.prompt,
                      });
                    }}
                    aria-label={t("settings.postProcessing.prompts.editNamed", {
                      defaultValue: "Edit {{name}}",
                      name: prompt.name,
                    })}
                    data-testid="prompt-edit"
                  >
                    <Pencil aria-hidden="true" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-gray-700 hover:text-red-900"
                    disabled={busy || isLastPrompt}
                    onClick={() => {
                      setError(null);
                      setPendingDelete(prompt);
                    }}
                    aria-label={t(
                      "settings.postProcessing.prompts.deleteNamed",
                      {
                        defaultValue: "Delete {{name}}",
                        name: prompt.name,
                      },
                    )}
                    data-testid="prompt-delete"
                  >
                    <Trash2 aria-hidden="true" />
                  </Button>
                </RowActions>
              </span>
            </RuleRow>
          );
        })}
      </RuleList>
    );
  };

  return (
    <>
      <SettingsSection label={sectionLabel}>
        <div
          className="divide-y divide-gray-alpha-400"
          data-testid="dictation-prompt-list"
        >
          {library()}

          {/* Two states the list cannot show on its own: nothing is selected,
           * or the last prompt is the reason delete is unavailable. */}
          {prompts.length > 0 && selectedId === null && (
            <Notice live={false} className="px-6 py-3">
              {t(
                "settings.postProcessing.prompts.noSelection",
                "No prompt selected: every mode uses the prompt it defines.",
              )}
            </Notice>
          )}

          {prompts.length === 1 && (
            <Notice live={false} className="px-6 py-3">
              {t(
                "settings.postProcessing.prompts.lastPrompt",
                "Sona keeps at least one prompt, so this one cannot be deleted. Create another first.",
              )}
            </Notice>
          )}

          {/* The confirm dialog covers this region, so a failed delete is
           * repeated inside the dialog instead. */}
          {error && pendingDelete === null && (
            <div className="flex flex-wrap items-center justify-between gap-3 px-6 py-3">
              <Notice tone="danger">{error}</Notice>
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() => {
                  setError(null);
                  void refreshSettings();
                }}
              >
                {t("common.retry")}
              </Button>
            </div>
          )}
        </div>
      </SettingsSection>

      <Dialog
        open={draft !== null}
        onOpenChange={(open) => {
          if (!open) {
            setDraft(null);
            setDraftError(null);
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {draft?.id === ""
                ? t("settings.postProcessing.prompts.createNew")
                : t("settings.postProcessing.prompts.updatePrompt")}
            </DialogTitle>
          </DialogHeader>
          {draft && (
            <div className="flex flex-col gap-4">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor={nameFieldId}>
                  {t("settings.postProcessing.prompts.promptLabel")}
                </Label>
                <Input
                  id={nameFieldId}
                  value={draft.name}
                  onChange={(event) =>
                    setDraft({ ...draft, name: event.target.value })
                  }
                  placeholder={t(
                    "settings.postProcessing.prompts.promptLabelPlaceholder",
                  )}
                  disabled={busy}
                  data-testid="prompt-name-field"
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor={bodyFieldId}>
                  {t("settings.postProcessing.prompts.promptInstructions")}
                </Label>
                <Textarea
                  id={bodyFieldId}
                  className={`min-h-40 ${literalText}`}
                  rows={8}
                  value={draft.prompt}
                  onChange={(event) =>
                    setDraft({ ...draft, prompt: event.target.value })
                  }
                  placeholder={t(
                    "settings.postProcessing.prompts.promptInstructionsPlaceholder",
                  )}
                  aria-describedby={outputTipId}
                  disabled={busy}
                  data-testid="prompt-body-field"
                />
                {/* The one thing the field cannot show: the token that marks
                 * where the transcript lands. */}
                <Hint id={outputTipId}>
                  {t(
                    "settings.postProcessing.prompts.outputTip",
                    "Write ${output} where the transcript should be inserted.",
                  )}
                </Hint>
              </div>
              {draftError && <Notice tone="danger">{draftError}</Notice>}
            </div>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => {
                setDraft(null);
                setDraftError(null);
              }}
            >
              {t("common.cancel")}
            </Button>
            <Button
              size="sm"
              disabled={busy || draftIncomplete}
              onClick={saveDraft}
              data-testid="prompt-save"
            >
              {draft?.id === ""
                ? t("settings.postProcessing.prompts.createPrompt")
                : t("settings.postProcessing.prompts.updatePrompt")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {t("settings.postProcessing.prompts.deletePrompt")}
            </DialogTitle>
            {/* The consequence is the description, so the dialog is a title
             * and one sentence rather than a title, a blurb and a sentence. */}
            <DialogDescription>
              {pendingDelete?.id === selectedId
                ? t("settings.postProcessing.prompts.confirmDelete.selected", {
                    defaultValue:
                      "{{name}} is in use. Deleting it moves the selection to the first prompt in the list.",
                    name: pendingDelete?.name ?? "",
                  })
                : t("settings.postProcessing.prompts.confirmDelete.body", {
                    defaultValue:
                      "{{name}} is removed from the library. Modes that already copied its text keep their own prompt.",
                    name: pendingDelete?.name ?? "",
                  })}
            </DialogDescription>
          </DialogHeader>
          {error && <Notice tone="danger">{error}</Notice>}
          <DialogFooter>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => setPendingDelete(null)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              variant="destructive"
              size="sm"
              disabled={busy}
              onClick={() => {
                const target = pendingDelete;
                if (!target) return;
                void runCommand(
                  () => commands.deletePostProcessPrompt(target.id),
                  setError,
                  () => setPendingDelete(null),
                );
              }}
              data-testid="prompt-delete-confirm"
            >
              {t("common.delete")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
};
