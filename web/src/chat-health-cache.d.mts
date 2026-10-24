import type { ChatHealth } from "./generated/ChatHealth";
interface ChatHealthCache {
  get(key: string): ChatHealth | null;
  set(key: string, health: ChatHealth): void;
  delete(key: string): void;
}
export function createChatHealthCache(ttl?: number, now?: () => number): ChatHealthCache;
export const chatHealthCache: ChatHealthCache;
