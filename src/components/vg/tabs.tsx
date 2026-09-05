"use client";

import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { Tabs as TabsPrimitive } from "radix-ui";

import { cn } from "@/lib/cn";

function Tabs({
  className,
  orientation = "horizontal",
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Root>) {
  return (
    <TabsPrimitive.Root
      data-slot="tabs"
      data-orientation={orientation}
      orientation={orientation}
      className={cn(
        "group/tabs flex gap-2 data-[orientation=horizontal]:flex-col",
        className,
      )}
      {...props}
    />
  );
}

/* One list look, because the app only ever asked for one. Every `TabsList` in
 * src/ passed `variant="line"` — Settings, the meeting review, Modes and the
 * agent workspace — and the boxed `bg-muted` strip the kit shipped as its
 * default had no caller at all. The segmented look lives in ToggleGroup, where
 * Library's Processed·Raw filter and the two settings pickers actually build
 * it; a value filter is not navigation, and the two were never the same
 * control. The `variant` prop stays so those four call sites keep compiling,
 * and their `variant="line"` is now a no-op their owners can drop. */
const tabsListVariants = cva(
  "group/tabs-list inline-flex w-fit items-center justify-start gap-6 bg-transparent p-[3px] group-data-[orientation=horizontal]/tabs:h-9 group-data-[orientation=vertical]/tabs:h-fit group-data-[orientation=vertical]/tabs:flex-col",
  {
    variants: {
      variant: {
        line: "",
      },
    },
    defaultVariants: {
      variant: "line",
    },
  },
);

function TabsList({
  className,
  variant = "line",
  ...props
}: React.ComponentProps<typeof TabsPrimitive.List> &
  VariantProps<typeof tabsListVariants>) {
  return (
    <TabsPrimitive.List
      data-slot="tabs-list"
      data-variant={variant}
      className={cn(tabsListVariants({ variant }), className)}
      {...props}
    />
  );
}

function TabsTrigger({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      data-slot="tabs-trigger"
      /* The recipe SettingsHub and MeetingReview each wrote out by hand, moved
       * here so a page that just renders tabs gets it: a word at body size in
       * the secondary ink, the active one at full ink and one weight up, with a
       * 1px --color-primary rule under it. No plate, no border, no pill — the
       * label carries the state.
       *
       * `flex-none px-0`: text tabs are words on a rule, spaced by the list's
       * gap. The kit's `flex-1` stretched three words across the pane and put
       * the underline under the whitespace either side of each one.
       *
       * No focus classes. base.css paints the app's one 2px bronze outline on
       * `:focus-visible`, and a `<button>` is exactly what that rule selects. */
      className={cn(
        "relative inline-flex h-[calc(100%-1px)] flex-none items-center justify-center gap-1.5 px-0 text-[14px] leading-[21px] font-normal whitespace-nowrap text-gray-900 transition-colors group-data-[orientation=vertical]/tabs:w-full group-data-[orientation=vertical]/tabs:justify-start hover:text-gray-1000 disabled:pointer-events-none disabled:opacity-50 motion-reduce:transition-none data-[state=active]:font-medium data-[state=active]:text-gray-1000 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
        "after:absolute after:bg-primary after:opacity-0 after:transition-opacity group-data-[orientation=horizontal]/tabs:after:inset-x-0 group-data-[orientation=horizontal]/tabs:after:bottom-[-5px] group-data-[orientation=horizontal]/tabs:after:h-px group-data-[orientation=vertical]/tabs:after:inset-y-0 group-data-[orientation=vertical]/tabs:after:-right-1 group-data-[orientation=vertical]/tabs:after:w-px data-[state=active]:after:opacity-100 motion-reduce:after:transition-none",
        className,
      )}
      {...props}
    />
  );
}

function TabsContent({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Content>) {
  return (
    <TabsPrimitive.Content
      data-slot="tabs-content"
      /* No `outline-none` here: Radix puts `tabindex=0` on the panel, so it is
       * a Tab stop, and styles/base.css paints `[tabindex]:focus-visible`. The
       * utility layer outranks that base rule, so the word left every tab
       * panel in the app a stop that showed the reader nothing.
       *
       * The ring is inset because a panel fills its pane: at the app's default
       * offset the left edge lands under the rail and the right edge lands
       * outside the window, and the reader sees two horizontal rules instead
       * of a rectangle. */
      className={cn("flex-1 focus-visible:-outline-offset-2", className)}
      {...props}
    />
  );
}

export { Tabs, TabsList, TabsTrigger, TabsContent, tabsListVariants };
