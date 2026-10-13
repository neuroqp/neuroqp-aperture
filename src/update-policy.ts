export const checkInterval = 24 * 60 * 60 * 1000;

export type UpdatePreferences = {
  enabled: boolean;
  lastAttempt: number;
  deferredVersion: string;
};

export function updatePreferences(raw: string | null): UpdatePreferences {
  let saved: Partial<UpdatePreferences> = {};
  try {
    saved = JSON.parse(raw ?? "{}");
  } catch {
    // A damaged preference never prevents opening the viewer.
  }
  return {
    enabled: saved?.enabled !== false,
    lastAttempt:
      Number.isFinite(saved?.lastAttempt) && Number(saved.lastAttempt) >= 0
        ? Number(saved.lastAttempt)
        : 0,
    deferredVersion:
      typeof saved?.deferredVersion === "string" ? saved.deferredVersion : "",
  };
}

export function automaticCheckDue(preferences: UpdatePreferences, now: number) {
  return (
    preferences.enabled &&
    (preferences.lastAttempt === 0 ||
      now < preferences.lastAttempt ||
      now - preferences.lastAttempt >= checkInterval)
  );
}

export function announceUpdate(
  version: string,
  preferences: UpdatePreferences,
  manual: boolean,
) {
  return manual || version !== preferences.deferredVersion;
}
