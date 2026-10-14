import * as React from "react";
import type { RelayEvent } from "@/shared/api/types";
import { Button } from "@/shared/ui/button";
import { Textarea } from "@/shared/ui/textarea";
import { EconomyField } from "./ProjectEconomyForms";
import {
  economyAmount,
  economyBody,
  economyText,
  policyAuthorities,
  type ProjectEconomy,
  type SubmitEconomyAction,
} from "../economy";

function SignedRecord({ event }: { event: RelayEvent }) {
  return (
    <details className="text-xs text-muted-foreground">
      <summary className="cursor-pointer">Signed record</summary>
      <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted p-3">
        {JSON.stringify(event, null, 2)}
      </pre>
    </details>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <p className="py-4 text-sm text-muted-foreground">{children}</p>;
}

export function ContributionRecords({
  data,
  pending,
  submit,
}: {
  data: ProjectEconomy;
  pending: boolean;
  submit: SubmitEconomyAction;
}) {
  return (
    <div className="space-y-5">
      <section className="space-y-3">
        <h3 className="font-semibold">Award proposals</h3>
        {data.proposals.length === 0 && <Empty>No award proposals yet.</Empty>}
        {data.proposals.map((event) => {
          const body = economyBody(event);
          const policyId = event.tags.find(
            (tag) => tag[0] === "st8-policy-event",
          )?.[1];
          const claimId = event.tags.find((tag) => tag[0] === "e")?.[1];
          const claim = data.claims.find(
            (candidate) => candidate.id === claimId,
          );
          const policy = data.policies.find(
            (candidate) => candidate.id === policyId,
          );
          const authorized =
            claim &&
            policy &&
            data.actionable_proposal_ids.includes(event.id) &&
            policyAuthorities(policy).includes(`nostr:${data.viewer_pubkey}`);
          const approved = data.approvals.some(
            (approval) =>
              approval.pubkey === data.viewer_pubkey &&
              approval.tags.some(
                (tag) => tag[0] === "e" && tag[1] === event.id,
              ),
          );
          const recorded = data.contribution_ledger.some(
            (entry) =>
              economyBody(entry).decision_proposal_event_id === event.id,
          );
          const expired =
            typeof body.decided_at !== "number" ||
            body.decided_at <= Date.now() / 1000;
          return (
            <article className="space-y-2 rounded-xl border p-3" key={event.id}>
              <p className="text-sm font-medium">
                Proposed: {economyText(body.status)} ·{" "}
                {economyAmount(body.contribution_units)} CU
              </p>
              <p className="whitespace-pre-wrap text-sm">
                {economyText(body.rationale)}
              </p>
              <p className="text-sm">
                Claim:{" "}
                {claim
                  ? economyText(economyBody(claim).summary)
                  : "Not available in this view"}
              </p>
              {claim && (
                <p className="break-all text-xs">Contributor: {claim.pubkey}</p>
              )}
              <p className="text-xs text-muted-foreground">
                Policy {economyText(body.policy_version)} ·{" "}
                {typeof body.decided_at === "number"
                  ? `Closes ${new Date(body.decided_at * 1000).toLocaleString()}`
                  : "Missing deadline"}
              </p>
              {recorded ? (
                <p className="text-sm">Recorded in the ledger</p>
              ) : approved ? (
                <p className="text-sm">
                  Your approval is signed; waiting for the governance threshold.
                </p>
              ) : authorized && !expired ? (
                <Button
                  disabled={pending}
                  onClick={() =>
                    void submit({ type: "approve", proposal_id: event.id })
                  }
                  size="sm"
                >
                  Sign approval for this award
                </Button>
              ) : (
                <p className="text-xs text-muted-foreground">
                  {expired
                    ? "Approval window closed"
                    : "Approval requires a verified proposal, its exact claim and policy, and an authorized signing identity."}
                </p>
              )}
              <SignedRecord event={event} />
            </article>
          );
        })}
      </section>
      <section className="space-y-3">
        <h3 className="font-semibold">Contribution ledger</h3>
        <p className="text-sm text-muted-foreground">
          Contribution Units record accepted work. They are not money or
          ownership. Anchor status is the relay’s testnet observation.
        </p>
        {data.contribution_ledger.length === 0 && (
          <Empty>
            No recorded awards yet. Submit a claim, propose an award, then
            collect the policy’s required approvals.
          </Empty>
        )}
        {data.contribution_ledger.map((event) => {
          const body = economyBody(event);
          return (
            <article className="space-y-2 rounded-xl border p-3" key={event.id}>
              <p className="text-sm font-medium">
                {economyAmount(body.contribution_units)} CU ·{" "}
                {economyText(body.status)}
              </p>
              <p className="break-all text-xs">
                Contributor: {economyText(body.contributor)}
              </p>
              <p className="text-sm">
                Testnet anchor: {economyText(body.anchor_state, "Pending")}
              </p>
              <SignedRecord event={event} />
            </article>
          );
        })}
      </section>
      <section className="space-y-3">
        <h3 className="font-semibold">Submitted claims</h3>
        {data.claims.length === 0 && <Empty>No contribution claims yet.</Empty>}
        {data.claims.map((event) => (
          <article className="space-y-2 rounded-xl border p-3" key={event.id}>
            <p className="whitespace-pre-wrap text-sm">
              {economyText(economyBody(event).summary)}
            </p>
            <SignedRecord event={event} />
          </article>
        ))}
      </section>
    </div>
  );
}

function ComputeReceipt({
  event,
  data,
  pending,
  submit,
}: {
  event: RelayEvent;
  data: ProjectEconomy;
  pending: boolean;
  submit: SubmitEconomyAction;
}) {
  const [reason, setReason] = React.useState("");
  const body = economyBody(event);
  const owner = data.viewer_pubkey === data.project_event.pubkey;
  const requester = body.requester === `nostr:${data.viewer_pubkey}`;
  const receiptId = economyText(body.receipt_id, "");
  const dispute = data.compute_disputes.find((candidate) =>
    candidate.tags.some((tag) => tag[0] === "d" && tag[1] === receiptId),
  );
  const canDispute =
    (owner || requester) &&
    body.dispute_state === "undisputed" &&
    !body.settlement_id;
  const canResolve = owner && body.dispute_state === "disputed" && dispute;
  const decide = (outcome: string) =>
    void submit(
      {
        type: "resolve",
        receipt_id: receiptId,
        dispute_event_id: dispute?.id,
        outcome,
        reason: reason.trim(),
      },
      true,
    );
  return (
    <article className="space-y-2 rounded-xl border p-3">
      <p className="text-sm font-medium">
        {economyText(body.model)} ·{" "}
        {economyAmount(body.billable_cost_sats ?? body.cost_sats)} sats
      </p>
      <p className="text-sm">
        {economyText(body.status)} · {economyText(body.dispute_state)} ·{" "}
        {body.settlement_id ? "Included in settlement" : "Unsettled"}
      </p>
      <p className="break-all text-xs">
        Provider: {economyText(body.provider)}
      </p>
      {(canDispute || canResolve) && (
        <div className="space-y-2">
          <EconomyField label="Public dispute or resolution reason">
            <Textarea
              required
              value={reason}
              onChange={(change) => setReason(change.target.value)}
              maxLength={4000}
            />
          </EconomyField>
          {canDispute && (
            <Button
              disabled={pending || !reason.trim()}
              onClick={() =>
                void submit(
                  {
                    type: "dispute",
                    receipt_id: receiptId,
                    reason: reason.trim(),
                  },
                  true,
                )
              }
              size="sm"
              variant="outline"
            >
              Sign dispute
            </Button>
          )}
          {canResolve && (
            <>
              <p className="text-xs text-muted-foreground">
                As project owner, your signed decision is final: retain the
                original charge or void it.
              </p>
              <div className="flex gap-2">
                <Button
                  disabled={pending || !reason.trim()}
                  onClick={() => decide("uphold_charge")}
                  size="sm"
                  variant="outline"
                >
                  Uphold charge
                </Button>
                <Button
                  disabled={pending || !reason.trim()}
                  onClick={() => decide("void_charge")}
                  size="sm"
                  variant="outline"
                >
                  Void charge
                </Button>
              </div>
            </>
          )}
        </div>
      )}
      <SignedRecord event={event} />
    </article>
  );
}

export function ComputeRecords({
  data,
  pending,
  submit,
}: {
  data: ProjectEconomy;
  pending: boolean;
  submit: SubmitEconomyAction;
}) {
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        Private-beta compute accounting uses testnet commitments. Recorded sats
        and settlement anchors do not mean a provider has been paid. Local
        inference runs through the st8-compute CLI.
      </p>
      <h3 className="font-semibold">Compute receipts</h3>
      {data.compute_receipts.length === 0 && (
        <Empty>No compute receipts for this project yet.</Empty>
      )}
      {data.compute_receipts.map((event) => (
        <ComputeReceipt
          data={data}
          event={event}
          key={event.id}
          pending={pending}
          submit={submit}
        />
      ))}
      <h3 className="font-semibold">Settlement commitments</h3>
      {data.compute_settlements.length === 0 && (
        <Empty>No settlement commitments yet.</Empty>
      )}
      {data.compute_settlements.map((event) => {
        const body = economyBody(event);
        return (
          <article className="space-y-2 rounded-xl border p-3" key={event.id}>
            <p className="text-sm">
              Testnet anchor: {economyText(body.anchor_state, "Pending")}
            </p>
            <SignedRecord event={event} />
          </article>
        );
      })}
    </div>
  );
}
