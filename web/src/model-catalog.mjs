const latestMarker = /\b(latest|preview|current)\b/gi;

function describeModel(model) {
  const name = (model.name ?? "").toLowerCase();
  const id = model.modelId.toLowerCase();
  const familySource = (name || id).replace(/20\d{6}/g, " ").replace(latestMarker, " ");
  const versionSource = name || id;
  const snapshot = id.match(/20\d{6}/)?.[0] ?? "";
  const versionMatch = versionSource.replace(/20\d{6}/g, " ").match(/\d+(?:[.-]\d+)*/) ?? id.replace(/20\d{6}/g, " ").match(/\d+(?:[.-]\d+)*/);
  const version = versionMatch ? versionMatch[0].split(/[.-]/).map(Number) : [];
  const family = familySource
    .replace(/\d+(?:[.-]\d+)*/g, " ")
    .replace(/[^a-z]+/g, " ")
    .trim()
    .replace(/\s+/g, " ") || model.modelId.toLowerCase();
  return { family, version, hasVersion: version.length > 0, snapshot };
}

function compareVersion(a, b) {
  const length = Math.max(a.length, b.length);
  for (let i = 0; i < length; i += 1) {
    const difference = (a[i] ?? 0) - (b[i] ?? 0);
    if (difference) return difference;
  }
  return 0;
}

/** Pick one concrete, highest-version model from each provider and family. */
export function latestModels(models) {
  const groups = new Map();
  for (const model of models) {
    const { family, version, hasVersion, snapshot } = describeModel(model);
    const key = `${model.provider.toLowerCase()}\0${family}`;
    const current = groups.get(key);
    const candidate = { model, version, hasVersion, snapshot };
    if (!current) { groups.set(key, candidate); continue; }
    const order = Number(candidate.hasVersion) - Number(current.hasVersion)
      || compareVersion(candidate.version, current.version)
      || candidate.snapshot.localeCompare(current.snapshot);
    if (order > 0 || (order === 0 && model.modelId.length < current.model.modelId.length)) groups.set(key, candidate);
  }
  return [...groups.values()].map(({ model }) => model).sort((a, b) =>
    a.provider.localeCompare(b.provider) || (a.name ?? a.modelId).localeCompare(b.name ?? b.modelId),
  );
}
