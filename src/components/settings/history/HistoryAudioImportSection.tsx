import React from "react";
import { useTranslation } from "react-i18next";
import { FileAudio, Loader2 } from "lucide-react";
import type { AudioImportJob } from "@/bindings";
import { Microlabel, SETTINGS_CARD } from "../rows";
import { Button } from "@/components/vg/button";
import { cn } from "@/lib/cn";
import { IMPORT_RUNNING } from "./audioImportJobs";

interface HistoryAudioImportSectionProps {
  jobs: AudioImportJob[];
  error: "start" | "cancel" | "load" | null;
  onCancel: (job: AudioImportJob) => void;
}

/* `done` is two sentences, not one: a dictation landed in History, and a
 * recording the operating system opened landed in Library. The job's result
 * says which of the two happened; its status only says it finished. */
const statusKey = (job: AudioImportJob): string =>
  job.result?.kind === "meeting"
    ? "settings.history.audioImport.status.savedAsMeeting"
    : `settings.history.audioImport.status.${job.status}`;

/**
 * The file imports still running, on the page they will land on.
 *
 * The import dialog shows the same jobs while it is open; this is where they
 * keep reporting once it is closed, so the two lists are drawn to one recipe —
 * a light row on the sunken step, a 16px glyph, the name, then what the job is
 * doing. A row here carries the one thing the dialog cannot: cancelling work
 * that outlived the dialog that started it.
 */
export const HistoryAudioImportSection: React.FC<
  HistoryAudioImportSectionProps
> = ({ jobs, error, onCancel }) => {
  const { t } = useTranslation();

  if (jobs.length === 0 && error === null) return null;

  return (
    <div className="flex flex-col gap-3">
      {error && (
        <p
          role="alert"
          className={cn(SETTINGS_CARD, "px-4 py-3 text-sm text-red-900")}
        >
          {t(`settings.history.audioImport.errors.${error}`)}
        </p>
      )}

      {jobs.length > 0 && (
        <section
          aria-labelledby="audio-import-jobs-title"
          data-testid="history-imports"
          className="flex flex-col gap-2"
        >
          <h2 id="audio-import-jobs-title">
            <Microlabel>{t("settings.history.audioImport.jobs")}</Microlabel>
          </h2>
          <ol className="flex flex-col gap-2">
            {jobs.map((job) => {
              const canCancel =
                !job.cancel_requested && job.status in IMPORT_RUNNING;
              const failure =
                job.result?.kind === "failed" ? job.result.code : null;
              const status = job.cancel_requested
                ? t("settings.history.audioImport.status.cancelling")
                : t(statusKey(job));
              return (
                <li
                  key={job.id}
                  className="flex flex-col gap-1 rounded-md bg-surface-sunken px-4 py-3"
                >
                  <div className="flex items-center gap-3 text-[14px] leading-[21px] text-gray-1000">
                    <FileAudio
                      aria-hidden="true"
                      className="size-4 shrink-0 text-gray-800"
                    />
                    <span
                      className="min-w-0 flex-1 truncate"
                      title={job.file_name}
                    >
                      {job.file_name}
                    </span>
                    {canCancel && (
                      <Loader2
                        aria-hidden="true"
                        className="size-4 shrink-0 animate-spin text-gray-800 motion-reduce:animate-none"
                      />
                    )}
                    {/* A failure states itself on the line below, so the
                     * trailing slot stays a single quiet word or nothing. */}
                    {failure === null && (
                      <span className="shrink-0 text-[13px] leading-[18px] text-gray-900">
                        {status}
                      </span>
                    )}
                    {canCancel && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => void onCancel(job)}
                        data-testid="history-import-cancel"
                      >
                        {t("settings.history.audioImport.cancel")}
                      </Button>
                    )}
                  </div>
                  {failure !== null && (
                    <p
                      role="alert"
                      className="text-[13px] leading-[18px] text-red-900"
                    >
                      {t(`settings.history.audioImport.failure.${failure}`)}
                    </p>
                  )}
                </li>
              );
            })}
          </ol>
        </section>
      )}
    </div>
  );
};
