import type { ReactNode } from "react";
import type { ProviderId } from "./settings-types";
import { providerIds, providerLabel } from "./providers";

export function ProviderChoices({ selected, onChange, disabled, children }: {
  selected: ProviderId[];
  onChange: (next: ProviderId[]) => void;
  disabled?: (id: ProviderId) => boolean;
  children?: (id: ProviderId) => ReactNode;
}) {
  return <section><h3>Providers</h3>{providerIds.map(id => <div className="provider" key={id}>
    <label><input type="checkbox" checked={selected.includes(id)} disabled={disabled?.(id)} onChange={e => onChange(e.target.checked ? [...selected, id] : selected.filter(p => p !== id))} />{providerLabel(id)}</label>
    {children?.(id)}
  </div>)}</section>;
}
