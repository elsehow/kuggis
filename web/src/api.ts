export const explain = (error: unknown) => {
  const message = error instanceof Error ? error.message : String(error);
  try {
    const detail = JSON.parse(message.slice(message.indexOf("{")));
    if (typeof detail.error === "string") return detail.error;
    if (typeof detail.error?.message === "string") return detail.error.message;
  } catch { /* Plain text errors are already suitable for display. */ }
  return message;
};
export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, { ...init, headers: { "Content-Type": "application/json", ...init?.headers } });
  if (!response.ok) {
    const text = await response.text();
    try { throw new Error(JSON.parse(text).error ?? text); } catch (e) { if (e instanceof SyntaxError) throw new Error(text); throw e; }
  }
  return response.status === 204 ? undefined as T : response.json();
}
