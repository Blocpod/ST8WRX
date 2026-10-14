import * as React from "react";
import type { ProjectEconomy, SubmitEconomyAction } from "../economy";
import {
  economyBody,
  economyIds,
  economyInteger,
  economyText,
  policyAuthorities,
} from "../economy";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Textarea } from "@/shared/ui/textarea";

const SELECT =
  "w-full rounded-lg border border-input bg-background px-3 py-2 text-sm";
const CLASSES = [
  "Intellectual",
  "Architecture",
  "Engineering",
  "ProductDesign",
  "AgentWork",
  "Compute",
  "TestingSecurityReview",
  "ResearchData",
  "CommercialDistribution",
  "Capital",
];

export function EconomyField({
  label,
  children,
}: {
  label: string;
  children: React.ReactElement<{ id?: string }>;
}) {
  const id = React.useId();
  return (
    <div className="grid gap-1 text-sm">
      <label className="font-medium" htmlFor={id}>
        {label}
      </label>
      {React.cloneElement(children, { id })}
    </div>
  );
}

export function ProjectEconomyForms({
  data,
  pending,
  submit,
  onError,
}: {
  data: ProjectEconomy;
  pending: boolean;
  submit: SubmitEconomyAction;
  onError: (error: unknown) => void;
}) {
  const [mode, setMode] = React.useState("claim");
  const [evidence, setEvidence] = React.useState("");
  const authorizedPolicies = data.policies.filter((policy) =>
    policyAuthorities(policy).includes(`nostr:${data.viewer_pubkey}`),
  );
  const owner = data.viewer_pubkey === data.project_event.pubkey;
  const submitForm = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const text = (key: string) => String(form.get(key) ?? "").trim();
    const integer = (key: string, minimum = 0) =>
      economyInteger(text(key), key.replaceAll("_", " "), minimum);
    try {
      if (mode === "claim") {
        await submit({
          type: "claim",
          evidence_ids: economyIds(evidence),
          summary: text("summary"),
          contributor_kind: text("contributor_kind"),
          class: text("class"),
        });
      } else if (mode === "propose") {
        const status = text("status");
        await submit({
          type: "propose",
          claim_id: text("claim_id"),
          policy_event_id: text("policy_event_id"),
          status,
          units: status === "Rejected" ? 0 : integer("units", 1),
          rationale: text("rationale"),
          decision_window_secs: integer("decision_window_secs", 1),
        });
      } else {
        const founders = economyIds(text("founders")).map(
          (key) => `nostr:${key}`,
        );
        await submit({
          type: "policy",
          version: text("version"),
          founders,
          normal_threshold: integer("normal_threshold", 1),
          large_threshold: integer("large_threshold", 1),
          large_unit_threshold: integer("large_unit_threshold", 1),
        });
      }
    } catch (error) {
      onError(error);
    }
  };
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap gap-2">
        <Button
          aria-pressed={mode === "claim"}
          onClick={() => setMode("claim")}
          size="sm"
          variant="outline"
        >
          Submit contribution
        </Button>
        {authorizedPolicies.length > 0 && (
          <Button
            aria-pressed={mode === "propose"}
            onClick={() => setMode("propose")}
            size="sm"
            variant="outline"
          >
            Propose award
          </Button>
        )}
        {owner && (
          <Button
            aria-pressed={mode === "policy"}
            onClick={() => setMode("policy")}
            size="sm"
            variant="outline"
          >
            Governance policy
          </Button>
        )}
      </div>
      <form
        className="grid gap-3"
        key={mode}
        onSubmit={(event) => void submitForm(event)}
      >
        <fieldset className="grid gap-3" disabled={pending}>
          {mode === "claim" ? (
            <>
              <p className="text-sm text-muted-foreground">
                Claims and their signed evidence become public verification
                records. Select project work you are ready to share.
              </p>
              <EconomyField label="Contribution summary">
                <Textarea name="summary" required maxLength={4000} />
              </EconomyField>
              <div className="grid grid-cols-2 gap-3">
                <EconomyField label="Contribution class">
                  <select
                    className={SELECT}
                    defaultValue="Engineering"
                    name="class"
                  >
                    {CLASSES.map((value) => (
                      <option key={value}>{value}</option>
                    ))}
                  </select>
                </EconomyField>
                <EconomyField label="Contributor type">
                  <select className={SELECT} name="contributor_kind">
                    {["Human", "Agent", "ComputeNode", "Organization"].map(
                      (value) => (
                        <option key={value}>{value}</option>
                      ),
                    )}
                  </select>
                </EconomyField>
              </div>
              {data.evidence.length > 0 && (
                <EconomyField label="Add recent project evidence">
                  <select
                    className={SELECT}
                    value=""
                    onChange={(event) => {
                      const id = event.target.value;
                      if (id)
                        setEvidence(
                          (current) =>
                            `${current}${current.trim() ? "\n" : ""}${id}`,
                        );
                    }}
                  >
                    <option value="">Choose signed work…</option>
                    {data.evidence.map((event) => (
                      <option key={event.id} value={event.id}>
                        {new Date(event.created_at * 1000).toLocaleDateString()}{" "}
                        ·{" "}
                        {event.content.slice(0, 100) ||
                          `Event kind ${event.kind}`}
                      </option>
                    ))}
                  </select>
                </EconomyField>
              )}
              <EconomyField label="Evidence event IDs">
                <Textarea
                  required
                  value={evidence}
                  onChange={(event) => setEvidence(event.target.value)}
                  placeholder="One complete event ID per line"
                />
              </EconomyField>
              <Button type="submit">Sign and submit claim</Button>
            </>
          ) : mode === "propose" ? (
            <>
              <EconomyField label="Contribution claim">
                <select className={SELECT} name="claim_id" required>
                  {data.claims.map((event) => (
                    <option key={event.id} value={event.id}>
                      {economyText(economyBody(event).summary, event.id)}
                    </option>
                  ))}
                </select>
              </EconomyField>
              <EconomyField label="Governance policy">
                <select className={SELECT} name="policy_event_id" required>
                  {authorizedPolicies.map((event) => (
                    <option key={event.id} value={event.id}>
                      {economyText(economyBody(event).version)}
                    </option>
                  ))}
                </select>
              </EconomyField>
              <EconomyField label="Decision">
                <select className={SELECT} name="status">
                  <option>Accepted</option>
                  <option>Adjusted</option>
                  <option>Rejected</option>
                </select>
              </EconomyField>
              <EconomyField label="Contribution Units (zero for rejection)">
                <Input
                  defaultValue="100"
                  inputMode="numeric"
                  name="units"
                  required
                />
              </EconomyField>
              <EconomyField label="Approval window in seconds">
                <Input
                  defaultValue="86400"
                  inputMode="numeric"
                  name="decision_window_secs"
                  required
                />
              </EconomyField>
              <EconomyField label="Rationale">
                <Textarea name="rationale" required maxLength={4000} />
              </EconomyField>
              <Button disabled={data.claims.length === 0} type="submit">
                Sign award proposal
              </Button>
            </>
          ) : (
            <>
              <p className="text-sm text-muted-foreground">
                Choose the authorities who may approve awards. Publishing a new
                version keeps earlier decisions tied to their original policy.
              </p>
              <EconomyField label="Policy version">
                <Input
                  name="version"
                  placeholder="project-v1"
                  required
                  maxLength={256}
                />
              </EconomyField>
              <EconomyField label="Authority public keys">
                <Textarea
                  defaultValue={data.viewer_pubkey}
                  name="founders"
                  required
                />
              </EconomyField>
              <EconomyField label="Required approvals">
                <Input
                  defaultValue="1"
                  inputMode="numeric"
                  name="normal_threshold"
                  required
                />
              </EconomyField>
              <EconomyField label="Required approvals for large awards">
                <Input
                  defaultValue="1"
                  inputMode="numeric"
                  name="large_threshold"
                  required
                />
              </EconomyField>
              <EconomyField label="Large award threshold (CU)">
                <Input
                  defaultValue="1000"
                  inputMode="numeric"
                  name="large_unit_threshold"
                  required
                />
              </EconomyField>
              <Button type="submit">Sign governance policy</Button>
            </>
          )}
        </fieldset>
      </form>
    </div>
  );
}
