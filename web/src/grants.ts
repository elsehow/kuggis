import type { Grant } from "./api-types";

// Grants are sets of permissions. Object property order is not part of their
// identity; repo names are case-insensitive in the Rust Repo parser as well.
export function grantKey(grant: Grant): string {
  if ("provider" in grant) return JSON.stringify(["provider", grant.provider]);
  if ("code" in grant) return JSON.stringify(["code", grant.code.repo.toLowerCase(), grant.code.write, grant.code.via?.toLowerCase() ?? null]);
  if ("host" in grant) return JSON.stringify(["host", grant.host]);
  if ("publish" in grant) {
    const scope = grant.publish;
    return "namespace" in scope
      ? JSON.stringify(["publish", "namespace", scope.namespace])
      : JSON.stringify(["publish", "site", scope.site.namespace, scope.site.name]);
  }
  return JSON.stringify(["edge", grant.edge]);
}

export function sameGrants(a: Grant[], b: Grant[]): boolean {
  const keys = (grants: Grant[]) => [...new Set(grants.map(grantKey))].sort();
  return JSON.stringify(keys(a)) === JSON.stringify(keys(b));
}

export function publishLabel(scope: Extract<Grant, { publish: unknown }>["publish"]): string {
  return "namespace" in scope
    ? `Publish sites in namespace ${scope.namespace}`
    : `Publish site ${scope.site.namespace}/${scope.site.name}`;
}

// Settings currently edit providers, repositories and hosts. Preserve all other
// authority when saving those controls, including operator-managed publishing.
export function preservedGrants(grants: Grant[]): Grant[] {
  return grants.filter((g) => !("provider" in g || "code" in g || "host" in g));
}
