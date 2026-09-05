import React from "react";
import { SettingsPage } from "@/components/settings/rows";
import { Skeleton } from "@/components/vg/skeleton";
import { MeetingStartGate } from "../MeetingStartGate";
import type {
  MeetingGateScreenActions,
  MeetingGateScreenModel,
  MeetingLoadingScreenModel,
} from "../meetingTypes";

/* The detail view loads a whole snapshot. Both pages behind it open the same
 * way now - a title line, then lines of text - so the skeleton is that shape
 * and the swap does not jump. */
const MeetingDetailSkeleton: React.FC<{ label: string }> = ({ label }) => (
  <SettingsPage
    role="status"
    aria-label={label}
    header={
      <div className="flex items-center justify-between gap-4">
        <Skeleton className="h-8 w-72" />
        <Skeleton className="h-8 w-20" />
      </div>
    }
  >
    <div className="flex flex-col gap-3">
      {["w-full", "w-11/12", "w-3/4"].map((width) => (
        <Skeleton key={width} className={`h-3.5 ${width}`} />
      ))}
    </div>
  </SettingsPage>
);

type MeetingSessionScreenProps =
  | { model: MeetingLoadingScreenModel; actions: null }
  | { model: MeetingGateScreenModel; actions: MeetingGateScreenActions };

/** Loading and preflight are the two session states without a live or review
 * page. Both receive a model and the actions valid for that state. */
export const MeetingSessionScreen: React.FC<MeetingSessionScreenProps> = (
  props,
) => {
  if (props.actions === null) {
    return <MeetingDetailSkeleton label={props.model.label} />;
  }

  const { model, actions } = props;
  return (
    <MeetingStartGate
      snapshot={model.snapshot}
      options={model.options}
      refreshing={model.refreshing}
      starting={model.starting}
      onRefresh={actions.onRefresh}
      onCancel={actions.onCancel}
      onStart={actions.onStart}
    />
  );
};
