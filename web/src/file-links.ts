// Absolute Markdown paths refer to the selected cell, never the supervisor.
export function cellFilePath(href: string): string | null {
  let path = href;
  if (href.startsWith("file:///")) path = href.slice(7);
  else if (!href.startsWith("/") || href.startsWith("//")) return null;
  try { path = decodeURIComponent(path); } catch { return null; }
  if (!path.startsWith("/") || path.startsWith("//") || /[\x00-\x1f\x7f]/.test(path)) return null;
  // Editor-style line anchors are navigation hints, not part of the filename.
  return path.replace(/#L\d+(?:-L?\d+)?$/, "").replace(/:\d+(?::\d+)?$/, "");
}

export function fileViewUrl(path: string) {
  const url = new URL(location.href);
  url.searchParams.set("view", "files");
  url.searchParams.set("path", path);
  return url.pathname + url.search;
}

export const fileDownloadUrl = (base: string, path: string) => `${base}/files/download?${new URLSearchParams({ path })}`;
