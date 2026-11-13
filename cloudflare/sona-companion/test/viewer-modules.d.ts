declare module "*?viewer-behavior-test" {
  export function parseFragment(hash: string): Uint8Array;
  export function renderMarkdown(container: Element, source: string): void;
}

declare module "*?viewer-schema-test" {}

declare module "*?viewer-document-test" {
  export interface SharedBlock {
    type: "paragraph" | "item";
    text: string;
    meta?: string | null;
    time?: string | null;
  }
  export interface SharedDocument {
    title: string;
    include: string;
    notesOutOfDate: boolean;
    sections: { heading: string; blocks: SharedBlock[] }[];
  }
  export function parseShareDocument(source: string): SharedDocument;
  export function renderShareDocument(
    container: Element,
    shared: SharedDocument,
  ): void;
}

declare module "*?viewer-document-boot-test" {}

declare module "*?raw" {
  const source: string;
  export default source;
}

declare module "*?live-join-test" {}
declare module "*?live-reload-test" {}
declare module "*?live-order-test" {}
declare module "*?live-render-test" {}
declare module "*?live-edit-test" {}
declare module "*?live-ended-test" {}
declare module "*?live-host-test" {}
declare module "*?live-vector-test" {}
declare module "*?live-late-test" {}
declare module "*?live-unsent-test" {}
declare module "*?live-reserved-test" {}
declare module "*?live-removed-test" {}
