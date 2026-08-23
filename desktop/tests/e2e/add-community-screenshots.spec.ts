import { test } from "@playwright/test";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";

const OUTDIR = "test-results/add-community";
const COMMUNITIES = [
  {
    id: "ws-a",
    name: "Alpha",
    relayUrl: "ws://localhost:3000",
    addedAt: "2026-01-01T00:00:00.000Z",
  },
  {
    id: "ws-b",
    name: "Bravo",
    relayUrl: "ws://localhost:3001",
    addedAt: "2026-01-02T00:00:00.000Z",
  },
];

test.beforeEach(async ({ page }) => {
  await page.addInitScript((communities) => {
    window.localStorage.setItem(
      "buzz-communities",
      JSON.stringify(communities),
    );
    window.localStorage.setItem("buzz-active-community-id", communities[0].id);
  }, COMMUNITIES);
  await installMockBridge(page, {}, { skipCommunitySeed: true });
  await page.goto("/");
  await page.getByTestId("community-rail-add").click();
});

test("capture: add-community choices", async ({ page }) => {
  const dialog = page.getByTestId("add-community-dialog");
  await dialog.waitFor();
  await waitForAnimations(page);
  await dialog.screenshot({ path: `${OUTDIR}/01-choices.png` });
});

test("capture: join an existing community", async ({ page }) => {
  await page.getByTestId("add-community-join").click();
  const dialog = page.getByTestId("add-community-dialog");
  const communityUrl = page.getByLabel("Community URL or invite link");
  await communityUrl.fill("community.example.com");
  await page.getByTestId("community-api-token-reveal").waitFor({
    state: "detached",
  });
  await waitForAnimations(page);
  await dialog.screenshot({ path: `${OUTDIR}/02-join.png` });
});
