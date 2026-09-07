import React, { useCallback, useEffect, useId, useState } from "react";
import { Pencil, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type PromptOutput,
  type PromptTarget,
  type SavedPrompt,
  type SavedPromptList,
} from "@/bindings";
import {
  Microlabel,
  Notice,
  RowActions,
  SettingsDisclosure,
  SettingsSection,
} from "@/components/settings/rows";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { Textarea } from "@/components/vg/textarea";
import { meetingErrorKey } from "./meetingUtils";
import { promptTargetKeys } from "./promptTargets";

/* The prompts this Mac keeps, and the editor that writes one.
 *
 * A prompt is a question the operator wrote once. The three that ship are rows
 * in the same table, seeded by the migration: this surface cannot tell them
 * apart from one typed this morning, and neither can the store.
 *
 * Every write carries the shared revision, so a second window editing another
 * prompt fences this one. A rejection is not an error to apologise for — it is
 * "read again", which is what `refresh` does. */

const TARGETS = ["meeting", "person", "series"] as const;

/** A prompt being written, before it is a prompt. */
interface Draft {
  promptId: string | null;
  name: string;
  body: string;
  schema: string | null;
  target: PromptTarget;
}

const blankDraft = (): Draft => ({
  promptId: null,
  name: "",
  body: "",
  schema: null,
  target: "meeting",
});

const draftOf = (prompt: SavedPrompt): Draft => ({
  promptId: prompt.prompt_id,
  name: prompt.name,
  body: prompt.body,
  schema: prompt.output.kind === "schema" ? prompt.output.json_schema : null,
  target: prompt.target,
});

const outputOf = (draft: Draft): PromptOutput =>
  draft.schema === null
    ? { kind: "text" }
    : { kind: "schema", json_schema: draft.schema };

interface MeetingPromptsProps {
  createRequest: number | null;
  onCreateRequestHandled: (nonce: number) => void;
}

export const MeetingPrompts: React.FC<MeetingPromptsProps> = ({
  createRequest,
  onCreateRequestHandled,
}) => {
  const { t } = useTranslation();
  const [list, setList] = useState<SavedPromptList | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const result = await commands.savedPromptList();
      if (result.status === "error") {
        setError(t(meetingErrorKey(result.error)));
        return;
      }
      setList(result.data);
      setError(null);
    } catch {
      setError(t("meetings.errors.load"));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (createRequest === null) return;
    setDraft(blankDraft());
    onCreateRequestHandled(createRequest);
  }, [createRequest, onCreateRequestHandled]);

  const save = async (draft: Draft) => {
    if (!list) return;
    setSaving(true);
    setError(null);
    try {
      const result = await commands.savedPromptSave({
        operation_id: crypto.randomUUID(),
        prompt_id: draft.promptId,
        name: draft.name,
        body: draft.body,
        output: outputOf(draft),
        target: draft.target,
        expected_revision: list.revision,
      });
      if (result.status === "error") {
        setError(
          result.error === "invalid_request"
            ? t("prompts.invalid")
            : t(meetingErrorKey(result.error)),
        );
        return;
      }
      if (result.data.receipt.result === "rejected") {
        /* Somebody else moved the revision. The refusal changed nothing, so the
         * honest response is to show what is true now. */
        setError(t("prompts.rejected"));
      } else {
        setDraft(null);
      }
      setList(result.data.prompts);
    } catch {
      setError(t("prompts.saveFailed"));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (prompt: SavedPrompt) => {
    if (!list) return;
    setSaving(true);
    setError(null);
    try {
      const result = await commands.savedPromptDelete({
        operation_id: crypto.randomUUID(),
        prompt_id: prompt.prompt_id,
        expected_revision: list.revision,
      });
      if (result.status === "error") {
        setError(t(meetingErrorKey(result.error)));
        return;
      }
      if (result.data.receipt.result === "rejected") {
        setError(t("prompts.rejected"));
      }
      setList(result.data.prompts);
    } catch {
      setError(t("prompts.saveFailed"));
    } finally {
      setSaving(false);
    }
  };

  if (loading && list === null) {
    return (
      <SettingsSection label={t("prompts.title")}>
        <div className="px-6 py-3">
          <Microlabel>{t("prompts.loading")}</Microlabel>
        </div>
      </SettingsSection>
    );
  }

  return (
    <SettingsSection label={t("prompts.title")}>
      <div className="px-6 py-3">
        <Microlabel>{t("prompts.description")}</Microlabel>
      </div>
      {list?.prompts.length === 0 ? (
        <div className="px-6 py-2.5">
          <Microlabel>{t("prompts.empty")}</Microlabel>
        </div>
      ) : null}
      {/* One disclosure per prompt, like a series under Automations: the page
       * stays the same height however many questions the operator keeps, and
       * the body — the part worth reading — is one click away instead of
       * hidden behind an edit dialog. */}
      {list?.prompts.map((prompt) => (
        <SettingsDisclosure
          key={prompt.prompt_id}
          label={prompt.name}
          fact={t(promptTargetKeys[prompt.target])}
        >
          <div
            data-slot="prompt-body"
            className="flex items-start gap-3 px-6 py-2.5"
          >
            <p className="min-w-0 flex-1 whitespace-pre-wrap text-[14px] leading-[21px] text-gray-900">
              {prompt.body}
            </p>
            <RowActions>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                aria-label={t("prompts.edit", { name: prompt.name })}
                disabled={saving}
                onClick={() => setDraft(draftOf(prompt))}
              >
                <Pencil aria-hidden="true" className="size-3.5" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="text-red-900 hover:text-red-900"
                aria-label={t("prompts.delete", { name: prompt.name })}
                disabled={saving}
                onClick={() => void remove(prompt)}
              >
                <Trash2 aria-hidden="true" className="size-3.5" />
              </Button>
            </RowActions>
          </div>
        </SettingsDisclosure>
      ))}
      {error ? (
        <div role="alert" className="px-6 py-2.5">
          <Notice tone="danger" live={false}>
            {error}
          </Notice>
        </div>
      ) : null}
      {draft ? (
        <PromptEditor
          draft={draft}
          saving={saving}
          onChange={setDraft}
          onCancel={() => setDraft(null)}
          onSave={() => void save(draft)}
        />
      ) : null}
    </SettingsSection>
  );
};

interface PromptEditorProps {
  draft: Draft;
  saving: boolean;
  onChange: (draft: Draft) => void;
  onCancel: () => void;
  onSave: () => void;
}

/* One prompt, in a dialog. The schema field appears only when the output is a
 * schema, because a JSON textarea beside a prompt that answers in prose is a
 * field that can only be wrong. */
const PromptEditor: React.FC<PromptEditorProps> = ({
  draft,
  saving,
  onChange,
  onCancel,
  onSave,
}) => {
  const { t } = useTranslation();
  const nameId = useId();
  const bodyId = useId();
  const outputId = useId();
  const schemaId = useId();
  const targetId = useId();
  const complete =
    draft.name.trim() !== "" &&
    draft.body.trim() !== "" &&
    (draft.schema === null || draft.schema.trim() !== "");

  return (
    <Dialog open onOpenChange={(open) => (open ? undefined : onCancel())}>
      <DialogContent className="sm:max-w-[560px]">
        <DialogHeader>
          <DialogTitle>
            {draft.promptId === null ? t("prompts.new") : t("prompts.editing")}
          </DialogTitle>
          <DialogDescription>{t("prompts.editorHint")}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <Field id={nameId} label={t("prompts.name")}>
            <Input
              id={nameId}
              value={draft.name}
              disabled={saving}
              onChange={(event) =>
                onChange({ ...draft, name: event.target.value })
              }
            />
          </Field>
          <Field id={bodyId} label={t("prompts.body")}>
            <Textarea
              id={bodyId}
              rows={4}
              value={draft.body}
              disabled={saving}
              placeholder={t("prompts.bodyPlaceholder")}
              onChange={(event) =>
                onChange({ ...draft, body: event.target.value })
              }
            />
          </Field>
          <Field id={outputId} label={t("prompts.output")}>
            <Select
              value={draft.schema === null ? "text" : "schema"}
              disabled={saving}
              onValueChange={(value) =>
                onChange({ ...draft, schema: value === "text" ? null : "" })
              }
            >
              <SelectTrigger id={outputId} size="sm" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="text">{t("prompts.outputText")}</SelectItem>
                <SelectItem value="schema">
                  {t("prompts.outputSchema")}
                </SelectItem>
              </SelectContent>
            </Select>
          </Field>
          {draft.schema === null ? null : (
            <Field id={schemaId} label={t("prompts.schema")}>
              <Textarea
                id={schemaId}
                rows={5}
                value={draft.schema}
                disabled={saving}
                placeholder={t("prompts.schemaPlaceholder")}
                className="font-mono text-[13px]"
                onChange={(event) =>
                  onChange({ ...draft, schema: event.target.value })
                }
              />
            </Field>
          )}
          <Field id={targetId} label={t("prompts.targetLabel")}>
            <Select
              value={draft.target}
              disabled={saving}
              onValueChange={(value) => {
                const target = TARGETS.find((candidate) => candidate === value);
                if (target !== undefined) onChange({ ...draft, target });
              }}
            >
              <SelectTrigger id={targetId} size="sm" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TARGETS.map((target) => (
                  <SelectItem key={target} value={target}>
                    {t(promptTargetKeys[target])}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            disabled={saving}
            onClick={onCancel}
          >
            {t("common.cancel")}
          </Button>
          <Button type="button" disabled={saving || !complete} onClick={onSave}>
            {t("prompts.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

const Field: React.FC<{
  id: string;
  label: string;
  children: React.ReactNode;
}> = ({ id, label, children }) => (
  <div className="flex flex-col gap-2">
    <label htmlFor={id} className="text-[14px] text-gray-1000">
      {label}
    </label>
    {children}
  </div>
);
