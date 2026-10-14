import { expect, test, type Page } from "@playwright/test";
import { installMockBridge } from "../helpers/bridge";
import { waitForAnimations } from "../helpers/animations";

async function openEconomy(page: Page) {
  await installMockBridge(page);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.waitForFunction(() => Boolean(window.__TAURI_INTERNALS__));
  await page.evaluate(() => {
    const bridge = window.__TAURI_INTERNALS__;
    const original = bridge.invoke.bind(bridge);
    let data: Record<string, unknown> | undefined;
    const signed = (
      id: string,
      kind: number,
      content: unknown,
      tags: string[][] = [],
    ) => ({
      id: id.repeat(64),
      kind,
      content: JSON.stringify(content),
      tags,
      pubkey: "deadbeef".repeat(8),
      created_at: Math.floor(Date.now() / 1000),
      sig: "00".repeat(64),
    });
    bridge.invoke = async (command, payload) => {
      const args = payload as Record<string, unknown> | undefined;
      if (command === "get_st8_project_economy") {
        if (!data) {
          const project = String(args?.project);
          const owner = project.split(":")[1];
          data = {
            relay_url: "http://127.0.0.1:3000",
            viewer_pubkey: owner,
            project_event: { ...signed("1", 30621, ""), pubkey: owner },
            policies: [
              signed("2", 30624, {
                version: "beta-v1",
                founders: [`nostr:${owner}`],
                normal_threshold: 1,
                large_threshold: 1,
                large_unit_threshold: 1000,
              }),
            ],
            claims: [],
            proposals: [],
            actionable_proposal_ids: [],
            approvals: [],
            contribution_ledger: [],
            compute_receipts: [
              signed("6", 30628, {
                receipt_id: "7".repeat(64),
                requester: `nostr:${owner}`,
                provider: "nostr:provider",
                model: "Local model",
                cost_sats: 12,
                status: "Completed",
                dispute_state: "undisputed",
                settlement_id: null,
              }),
            ],
            compute_settlements: [],
            compute_disputes: [],
            compute_resolutions: [],
            evidence: [
              {
                ...signed("8", 1621, "Implemented the project feature"),
                content: "Implemented the project feature",
              },
            ],
            has_more: true,
          };
        }
        return data;
      }
      if (
        command === "st8_contribution_action" ||
        command === "st8_compute_action"
      ) {
        if (
          !data ||
          args?.expectedRelayUrl !== data.relay_url ||
          args.expectedSignerPubkey !== data.viewer_pubkey
        )
          throw new Error("Expected pinned identity and relay");
        const action = args.action as Record<string, unknown>;
        if (action.type === "claim") data.claims = [signed("3", 49801, action)];
        if (action.type === "propose") {
          data.actionable_proposal_ids = ["4".repeat(64)];
          data.proposals = [
            signed(
              "4",
              49802,
              {
                ...action,
                contribution_units: action.units,
                policy_version: "beta-v1",
                decided_at: Math.floor(Date.now() / 1000) + 3600,
              },
              [
                ["e", "3".repeat(64)],
                ["st8-policy-event", "2".repeat(64)],
              ],
            ),
          ];
        }
        if (action.type === "approve")
          data.contribution_ledger = [
            signed("5", 30625, {
              contribution_units: 100,
              status: "accepted",
              contributor: data.viewer_pubkey,
              anchor_state: "confirmed",
              decision_proposal_event_id: "4".repeat(64),
            }),
          ];
        if (action.type === "dispute" || action.type === "resolve") {
          data.compute_disputes = [
            signed("9", 49813, { reason: action.reason }, [
              ["d", "7".repeat(64)],
            ]),
          ];
          data.compute_receipts = [
            signed("6", 30628, {
              receipt_id: "7".repeat(64),
              requester: `nostr:${data.viewer_pubkey}`,
              model: "Local model",
              cost_sats: 12,
              billable_cost_sats: action.type === "resolve" ? 0 : 12,
              status: "Completed",
              dispute_state:
                action.type === "resolve" ? "resolved_upheld" : "disputed",
              settlement_id: null,
            }),
          ];
        }
        return {
          accepted: true,
          event_id: "a".repeat(64),
          message: "accepted",
        };
      }
      return original(command, payload);
    };
  });
  await page.getByTestId("open-projects-view").click();
  await page.getByTestId("projects-section-projects").click();
  await page
    .locator(
      '[data-testid="project-card-buzz"], [data-testid="project-row-buzz"]',
    )
    .first()
    .click();
  await page.getByTestId("open-project-economy").click();
  await expect(
    page.getByText("Showing the latest 100 records per kind.", {
      exact: false,
    }),
  ).toBeVisible();
}

test("project economy submits evidence, proposes and approves an award", async ({
  page,
}) => {
  await openEconomy(page);
  const dialog = page.getByTestId("project-economy-dialog");
  await expect(
    dialog.getByText("No recorded awards yet.", { exact: false }),
  ).toBeVisible();
  await dialog.getByRole("tab", { name: "Submit & govern" }).click();
  await dialog.getByLabel("Contribution summary").fill("Delivered the feature");
  await dialog
    .getByLabel("Add recent project evidence")
    .selectOption("8".repeat(64));
  await dialog.getByRole("button", { name: "Sign and submit claim" }).click();
  await expect(dialog.getByRole("status")).toContainText(
    "Signed event accepted",
  );
  await dialog
    .getByRole("button", { name: "Propose award", exact: true })
    .click();
  await dialog.getByLabel("Rationale").fill("Reviewed and accepted");
  await dialog.getByRole("button", { name: "Sign award proposal" }).click();
  await dialog.getByRole("tab", { name: "Contributions", exact: true }).click();
  await expect(
    dialog.getByText("Reviewed and accepted", { exact: true }),
  ).toBeVisible();
  await dialog
    .getByRole("button", { name: "Sign approval for this award" })
    .click();
  await expect(dialog.getByText("100 CU · accepted")).toBeVisible();
  await expect(dialog.getByText("Testnet anchor: confirmed")).toBeVisible();
  await waitForAnimations(page);
  await dialog.screenshot({ path: "test-results/st8-contribution-ledger.png" });
});

test("project owner can dispute and void a compute charge", async ({
  page,
}) => {
  await openEconomy(page);
  const dialog = page.getByTestId("project-economy-dialog");
  await dialog.getByRole("tab", { name: "Compute", exact: true }).click();
  await expect(dialog.getByText("Local model · 12 sats")).toBeVisible();
  await dialog
    .getByLabel("Public dispute or resolution reason")
    .fill("The returned work was unusable");
  await dialog
    .getByRole("button", { name: "Sign dispute", exact: true })
    .click();
  await expect(
    dialog.getByRole("button", { name: "Void charge" }),
  ).toBeVisible();
  await dialog
    .getByLabel("Public dispute or resolution reason")
    .fill("Reviewed evidence and agreed to void");
  await dialog
    .getByRole("button", { name: "Void charge", exact: true })
    .click();
  await expect(dialog.getByText("Local model · 0 sats")).toBeVisible();
  await expect(
    dialog.getByRole("button", { name: "Sign dispute", exact: true }),
  ).toHaveCount(0);
  await waitForAnimations(page);
  await dialog.screenshot({ path: "test-results/st8-compute-dispute.png" });
});
