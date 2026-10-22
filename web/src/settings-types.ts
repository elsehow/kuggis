// Kuggis-owned settings request and response contracts generated from Rust.
export type { Provider as ProviderId } from "./generated/Provider";
export type { AuthMethod } from "./generated/AuthMethod";
export type { CredentialState } from "./generated/CredentialState";
export type { Authentication } from "./generated/Authentication";
export type { AccountObservation } from "./generated/AccountObservation";
export type { ProviderFailure } from "./generated/ProviderFailure";
export type { ProviderFailureKind } from "./generated/ProviderFailureKind";
export type { ProviderConnection } from "./generated/ProviderConnection";
export type { ConnectionName } from "./generated/ConnectionName";
export type { ConnectionAuth } from "./generated/ConnectionAuth";
export type { ConnectionConfig } from "./generated/ConnectionConfig";
export type { SettingsView } from "./generated/SettingsView";
export type { SecretUpdate } from "./generated/SecretUpdate";
export type { ConnectionUpdate } from "./generated/ConnectionUpdate";

export const connectionNames = ["bigbrain", "portal"] as const;
