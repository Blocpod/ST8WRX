import assert from "node:assert/strict";
import test from "node:test";
import {
  economyAmount,
  economyBody,
  economyIds,
  economyInteger,
  latestEconomyEvents,
} from "./economy.ts";

test("economy amounts reject fractions, negative and unsafe integers", () => {
  assert.equal(economyInteger("100", "CU"), 100);
  for (const value of ["-1", "1.2", "1e3", "", "9007199254740993"]) {
    assert.throws(() => economyInteger(value, "CU"));
  }
  assert.match(economyAmount(Number.MAX_SAFE_INTEGER + 1), /exact amount/);
});

test("replaceable ledger entries retain newest state and deterministic ties", () => {
  const event = {
    id: "c",
    kind: 30628,
    pubkey: "owner",
    tags: [["d", "receipt"]],
    created_at: 1,
  };
  const newest = { ...event, id: "b", created_at: 2 };
  const tieWinner = { ...newest, id: "a" };
  assert.deepEqual(latestEconomyEvents([event, newest, tieWinner]), [
    tieWinner,
  ]);
  const otherAuthor = { ...event, pubkey: "other" };
  assert.equal(latestEconomyEvents([event, otherAuthor]).length, 2);
});

test("evidence input requires exact distinct event IDs", () => {
  const id = "ab".repeat(32);
  assert.deepEqual(economyIds(id.toUpperCase()), [id]);
  for (const input of ["", "123", `${id}\n${id}`])
    assert.throws(() => economyIds(input));
});

test("malformed accounting content cannot crash the record list", () => {
  for (const content of ["{", "null", "[]", "42"])
    assert.deepEqual(economyBody({ content }), {});
  assert.deepEqual(economyBody({ content: '{"cost_sats":2}' }), {
    cost_sats: 2,
  });
});
