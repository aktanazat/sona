import {
  ApiError,
  type ChangeRow,
  type CompanionClient,
  type DeviceIdentity,
  changeCursorAfter,
  snapshotHighWater,
} from "./client";
import {
  type Cluster,
  type VaultKeys,
  clusterHue,
  idempotencyKey,
  openManifest,
  sealCard,
} from "./objects";
import { type SorterModel, buildCard, classify } from "./sort";
import type { Feed, FeedObject, StateDir } from "./state";

const RETRY_BASE_MS = 60 * 1000;
const RETRY_CAP_MS = 6 * 60 * 60 * 1000;

export interface PassResult {
  sorted: number;
  synced: number;
}

export interface SorterDeps {
  client: CompanionClient;
  identity: DeviceIdentity;
  keys: VaultKeys;
  log: (line: string) => void;
  model: SorterModel;
  state: StateDir;
}

function retryDelay(count: number): number {
  return Math.min(RETRY_BASE_MS * 2 ** Math.min(count, 20), RETRY_CAP_MS);
}

/**
 * One device's view of the vault: the change feed folded into a head index,
 * and a card written for every thought that has none.
 */
export class Sorter {
  constructor(private readonly deps: SorterDeps) {}

  async pass(): Promise<PassResult> {
    const feed = await this.deps.state.feed();
    const synced = await this.drain(feed);
    const sorted = await this.sortUnfiled(feed);
    return { sorted, synced };
  }

  /** Fold every change since the cursor into the feed, one saved page at a time. */
  private async drain(feed: Feed): Promise<number> {
    let synced = 0;
    for (;;) {
      let page;
      try {
        page = await this.deps.client.changes(feed.cursor);
      } catch (error) {
        if (!(error instanceof ApiError) || error.code !== "cursor_expired") throw error;
        this.deps.log("change cursor expired; rebuilding the index from a snapshot");
        synced += await this.rebuild(feed);
        continue;
      }
      for (const change of page.changes) {
        await this.apply(feed, change);
        synced += 1;
      }
      feed.cursor = page.next_cursor;
      await this.deps.state.saveFeed(feed);
      if (!page.has_more) return synced;
    }
  }

  /**
   * Replace the index with the Worker's heads at one high water, then resume the
   * change feed right after it. The cursor is built in the Worker's own encoding
   * because the snapshot reply carries no resume cursor of its own.
   */
  private async rebuild(feed: Feed): Promise<number> {
    const objects: Record<string, FeedObject> = {};
    let highWater: string | null = null;
    let after: string | null = null;
    let synced = 0;
    for (;;) {
      const page = await this.deps.client.snapshot(highWater, after);
      highWater = page.high_water;
      for (const head of page.heads) {
        if (head.tombstone) continue;
        const known = feed.objects[head.object_id];
        objects[head.object_id] =
          known !== undefined && known.revision_id === head.revision_id
            ? known
            : await this.read(head);
        synced += 1;
      }
      after = page.after;
      if (!page.has_more) break;
    }
    feed.objects = objects;
    feed.cursor = changeCursorAfter(snapshotHighWater(highWater));
    await this.deps.state.saveFeed(feed);
    return synced;
  }

  private async apply(feed: Feed, change: ChangeRow): Promise<void> {
    if (change.tombstone) {
      delete feed.objects[change.object_id];
      delete feed.attempts[change.object_id];
      return;
    }
    if (feed.objects[change.object_id]?.revision_id === change.revision_id) return;
    feed.objects[change.object_id] = await this.read(change);
  }

  /** Classify one head by opening its manifest; an unreadable head is indexed as `other`. */
  private async read(head: ChangeRow): Promise<FeedObject> {
    const { client, keys, log } = this.deps;
    const revision = { kind: "other" as const, revision_id: head.revision_id };
    let opened;
    try {
      opened = await openManifest(keys, await client.manifest(head.object_id, head.revision_id));
    } catch (error) {
      log(`object ${head.object_id} is not readable: ${String(error)}`);
      return revision;
    }
    if (opened.kind === "thought") return { kind: "thought", revision_id: head.revision_id };
    if (opened.kind === "card") {
      const { cluster, thought_id } = opened.manifest;
      return {
        cluster: { key: cluster.key, name: cluster.name },
        kind: "card",
        revision_id: head.revision_id,
        thought_id,
      };
    }
    return revision;
  }

  private clusters(feed: Feed): Cluster[] {
    const byKey = new Map<string, Cluster>();
    for (const object of Object.values(feed.objects)) {
      if (object.kind !== "card" || byKey.has(object.cluster.key)) continue;
      byKey.set(object.cluster.key, {
        hue: clusterHue(object.cluster.key),
        key: object.cluster.key,
        name: object.cluster.name,
      });
    }
    return [...byKey.values()];
  }

  /** Write a card for every thought without one whose retry, if any, is due. */
  private async sortUnfiled(feed: Feed): Promise<number> {
    const filed = new Set<string>();
    for (const object of Object.values(feed.objects)) {
      if (object.kind === "card") filed.add(object.thought_id);
    }
    let sorted = 0;
    for (const [thoughtId, object] of Object.entries(feed.objects)) {
      if (object.kind !== "thought" || filed.has(thoughtId)) continue;
      const attempt = feed.attempts[thoughtId];
      const now = this.deps.client.nowUtcMs();
      if (attempt !== undefined && now < attempt.next_at_utc_ms) continue;
      try {
        const card = await this.file(feed, thoughtId, object.revision_id, now);
        feed.objects[card.objectId] = {
          cluster: card.cluster,
          kind: "card",
          revision_id: card.revisionId,
          thought_id: thoughtId,
        };
        delete feed.attempts[thoughtId];
        filed.add(thoughtId);
        sorted += 1;
        this.deps.log(`filed thought ${thoughtId} as "${card.title}" in ${card.cluster.name}`);
      } catch (error) {
        const count = (attempt?.count ?? 0) + 1;
        feed.attempts[thoughtId] = { count, next_at_utc_ms: now + retryDelay(count) };
        this.deps.log(`thought ${thoughtId} not filed (attempt ${count}): ${String(error)}`);
      }
      await this.deps.state.saveFeed(feed);
    }
    return sorted;
  }

  private async file(
    feed: Feed,
    thoughtId: string,
    revisionId: string,
    now: number,
  ): Promise<{ cluster: Cluster; objectId: string; revisionId: string; title: string }> {
    const { client, identity, keys, model } = this.deps;
    const opened = await openManifest(keys, await client.manifest(thoughtId, revisionId));
    if (opened.kind !== "thought") throw new Error("head is no longer a thought");
    const thought = opened.manifest;
    const clusters = this.clusters(feed);
    const card = buildCard({
      classification:
        thought.text.trim().length === 0 ? null : await classify(model, thought, clusters),
      clusters,
      model: model.model,
      sorterDeviceId: identity.deviceId,
      thought,
      thoughtId,
      thoughtRevisionId: revisionId,
      writtenAtUtcMs: now,
    });
    const sealed = await sealCard(keys, identity, card);
    const { objectId, revisionId: cardRevisionId, uploadId } = sealed.plan;
    const key = (step: string): Promise<string> =>
      idempotencyKey(["card", objectId, cardRevisionId, step]);
    await client.createUpload(sealed.plan, await key("create"));
    await client.putChunk(uploadId, 0, sealed.chunk, sealed.chunkDigest, await key("chunk-0"));
    await client.commitUpload(uploadId, await key("commit"));
    return { cluster: card.cluster, objectId, revisionId: cardRevisionId, title: card.title };
  }
}
