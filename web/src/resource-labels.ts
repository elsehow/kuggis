import type { ResourceEffect } from "./generated/ResourceEffect";
import type { ResourceOwner } from "./generated/ResourceOwner";
import type { ResourceCell } from "./generated/ResourceCell";
export const effectLabel: Record<ResourceEffect, string> = {
  public_access_stopped: "Public access will stop.",
  data_retained: "Stored data and versions will be retained.",
  ownership_transferred: "Responsibility moves to the selected owner; the resource ID and URL stay the same.",
  local_access_removed: "Existing local access will be removed. Ownership alone does not grant runtime access.",
  remote_revocation_requires_manual_action: "Provider credentials/access must be revoked manually; local removal does not prove remote revocation.",
  costs_may_continue: "The external resource remains running or allocated; charges may continue.",
};
export function ownerLabel(owner: ResourceOwner, cells: ResourceCell[]): string {
  return owner === "operator" ? "Operator" : cells.find(c => c.identity === owner.cell)?.name ?? `Former/unavailable cell (${owner.cell.slice(0, 12)}…)`;
}
