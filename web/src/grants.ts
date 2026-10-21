import type { Grant } from "./api-types";

// Grants are sets of permissions. Object property order is not part of their
// identity; repo names are case-insensitive in the Rust Repo parser as well.
export function grantKey(grant: Grant): string {
  if ("provider" in grant) return JSON.stringify(["provider", grant.provider]);
  if ("code" in grant) return JSON.stringify(["code", grant.code.repo.toLowerCase(), grant.code.write, grant.code.via?.toLowerCase() ?? null]);
  if ("host" in grant) return JSON.stringify(["host", grant.host]);
  return JSON.stringify(["edge", grant.edge]);
}

export function sameGrants(a: Grant[], b: Grant[]): boolean {
  const keys = (grants: Grant[]) => [...new Set(grants.map(grantKey))].sort();
  return JSON.stringify(keys(a)) === JSON.stringify(keys(b));
}
