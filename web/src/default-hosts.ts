import type { Grant } from "./api-types";

// Explicit grants proposed by the creation form, never implicit proxy access.
// Playwright's CDN redirects Chrome for Testing to Google Storage and other
// artifacts (including FFmpeg) to Microsoft's download service.
export const browserTestingHosts = [
  "registry.npmjs.org",
  "cdn.playwright.dev",
  "storage.googleapis.com",
  "playwright.download.prss.microsoft.com",
  "archive.ubuntu.com",
  "us.archive.ubuntu.com",
  "security.ubuntu.com",
] as const;

export function defaultHosts(parentGrants?: readonly Grant[]): string[] {
  if (!parentGrants || parentGrants.some(g => "network" in g && g.network === "public")) return [...browserTestingHosts];
  const allowed = new Set(parentGrants.flatMap(g => "host" in g ? [g.host] : []));
  return browserTestingHosts.filter(host => allowed.has(host));
}
