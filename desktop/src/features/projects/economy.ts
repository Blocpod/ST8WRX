import type { RelayEvent } from "@/shared/api/types";

export type ProjectEconomy = {
  project_event: RelayEvent;
  relay_url: string;
  viewer_pubkey: string;
  policies: RelayEvent[];
  claims: RelayEvent[];
  proposals: RelayEvent[];
  actionable_proposal_ids: string[];
  approvals: RelayEvent[];
  contribution_ledger: RelayEvent[];
  compute_receipts: RelayEvent[];
  compute_settlements: RelayEvent[];
  compute_disputes: RelayEvent[];
  compute_resolutions: RelayEvent[];
  evidence: RelayEvent[];
  has_more: boolean;
};

export function economyBody(event: RelayEvent): Record<string, unknown> {
  try {
    const value: unknown = JSON.parse(event.content);
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  } catch {
    return {};
  }
}

export function economyText(value: unknown, fallback = "Unavailable"): string {
  return typeof value === "string" ? value : fallback;
}

export function economyAmount(value: unknown): string {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value.toLocaleString()
    : "See exact amount in signed record";
}

export function economyInteger(
  value: string,
  label: string,
  minimum = 0,
): number {
  if (!/^\d+$/.test(value)) throw new Error(`${label} must be a whole number.`);
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < minimum) {
    throw new Error(
      `${label} must be between ${minimum} and ${Number.MAX_SAFE_INTEGER}.`,
    );
  }
  return number;
}

export function economyIds(value: string): string[] {
  const ids = value
    .split(/[\s,]+/)
    .filter(Boolean)
    .map((id) => id.toLowerCase());
  if (
    ids.length < 1 ||
    ids.length > 256 ||
    ids.some((id) => !/^[a-f0-9]{64}$/.test(id))
  ) {
    throw new Error("Enter between 1 and 256 complete event IDs.");
  }
  if (new Set(ids).size !== ids.length)
    throw new Error("Event IDs must be unique.");
  return ids;
}

export function policyAuthorities(event: RelayEvent): string[] {
  const founders = economyBody(event).founders;
  return Array.isArray(founders)
    ? founders.filter((value): value is string => typeof value === "string")
    : [];
}

export function latestEconomyEvents(events: RelayEvent[]): RelayEvent[] {
  const latest = new Map<string, RelayEvent>();
  for (const event of events) {
    const address =
      event.kind >= 30000 && event.kind < 40000
        ? `${event.kind}:${event.pubkey}:${event.tags.find((tag) => tag[0] === "d")?.[1] ?? ""}`
        : event.id;
    const previous = latest.get(address);
    if (
      !previous ||
      event.created_at > previous.created_at ||
      (event.created_at === previous.created_at && event.id < previous.id)
    ) {
      latest.set(address, event);
    }
  }
  return [...latest.values()].sort(
    (a, b) => b.created_at - a.created_at || a.id.localeCompare(b.id),
  );
}

export type EconomyAction = { type: string; [field: string]: unknown };
export type SubmitEconomyAction = (
  action: EconomyAction,
  compute?: boolean,
) => Promise<void>;
