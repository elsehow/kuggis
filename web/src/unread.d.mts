import type { PiThreadMetadata } from "@assistant-ui/react-pi";
export type Seen = Record<string, number>;
export function activityTime(row: PiThreadMetadata): number;
export function isUnread(row: PiThreadMetadata, seen: Seen): boolean;
export function baseline(rows: readonly PiThreadMetadata[]): Seen;
export function merge(a: Seen, b: Seen): Seen;
export function loadSeen(storage: Storage, key: string): Seen | null;
export function saveSeen(storage: Storage, key: string, seen: Seen): void;
