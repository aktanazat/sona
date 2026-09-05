import React, { useState } from "react";
import { ChevronDown, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { Document } from "@/bindings";
import {
  Microlabel,
  Notice,
  SettingsSection,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { formatEntryTimestamp } from "@/lib/utils/format";
import { PeopleConfirmDialog } from "./PeopleConfirmDialog";

interface PersonDocumentsProps {
  documents: Document[];
  loadFailed: boolean;
  pending: boolean;
  onDelete: (document: Document) => void;
}

export const PersonDocuments: React.FC<PersonDocumentsProps> = ({
  documents,
  loadFailed,
  pending,
  onDelete,
}) => {
  const { t } = useTranslation();
  const [deleting, setDeleting] = useState<Document | null>(null);

  /* Nothing imported, nothing here: the verb that imports the first document
   * is in the page's menu, so this section is only ever the list of what came
   * back. A failed load is not an absence, and still says so. */
  if (!loadFailed && documents.length === 0) return null;

  return (
    <SettingsSection label={t("people.detail.documents")}>
      {loadFailed ? (
        <div className="px-6 py-3.5">
          <Notice tone="danger">{t("people.detail.documentsLoadError")}</Notice>
        </div>
      ) : (
        <ul className="divide-y divide-gray-alpha-400">
          {documents.map((document) => (
            <li
              key={document.summary.id}
              data-slot="person-document"
              className="px-6 py-3.5"
            >
              {/* The whole row is the disclosure now. Delete lives under the
               * text it deletes rather than on the closed line: a catalogue of
               * imported context reads as a catalogue, and the one control
               * that can destroy an entry waits inside the entry you opened. */}
              <details className="group min-w-0">
                <summary className="flex cursor-pointer list-none items-start gap-4 rounded-md focus-visible:ring-2 focus-visible:ring-focus-ring focus-visible:outline-none [&::-webkit-details-marker]:hidden">
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[14px] leading-[21px] font-medium text-gray-1000">
                      {document.summary.title}
                    </span>
                    <span className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1">
                      <Microlabel>{document.summary.source_name}</Microlabel>
                      <Microlabel className="tabular-nums">
                        {formatEntryTimestamp(
                          document.summary.created_at_utc_ms,
                        )}
                      </Microlabel>
                    </span>
                  </span>
                  <ChevronDown
                    aria-hidden="true"
                    className="mt-0.5 size-4 flex-none text-gray-700 transition-transform motion-reduce:transition-none group-open:rotate-180"
                  />
                </summary>
                <pre className="mt-3 max-h-48 overflow-auto whitespace-pre-wrap border-t border-gray-alpha-400 pt-3 text-[13px] leading-[20px] text-gray-900 select-text">
                  {document.content}
                </pre>
                <div className="mt-2 flex justify-end">
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="text-red-900 hover:text-red-900"
                    disabled={pending}
                    onClick={() => setDeleting(document)}
                  >
                    <Trash2 aria-hidden="true" />
                    {t("people.detail.deleteDocument")}
                  </Button>
                </div>
              </details>
            </li>
          ))}
        </ul>
      )}

      <PeopleConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => {
          if (!open) setDeleting(null);
        }}
        title={t("people.detail.deleteDocumentTitle")}
        description={t("people.detail.deleteDocumentDescription", {
          document: deleting?.summary.title ?? "",
        })}
        confirmLabel={t("people.detail.deleteDocument")}
        pending={pending}
        destructive
        onConfirm={() => {
          if (deleting !== null) onDelete(deleting);
          setDeleting(null);
        }}
      />
    </SettingsSection>
  );
};
