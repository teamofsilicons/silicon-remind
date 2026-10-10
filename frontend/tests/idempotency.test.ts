import assert from "node:assert/strict";
import { test } from "node:test";
import { PendingMutations } from "../lib/client/idempotency";

test("ambiguous mutation outcomes keep their key; a known outcome permits a new action", () => {
  const mutations = new PendingMutations();
  const first = mutations.begin("create schedule A");
  for (const status of [0, 408, 429, 500, 502]) {
    mutations.settle("create schedule A", status);
    assert.equal(mutations.begin("create schedule A"), first);
  }
  assert.notEqual(mutations.begin("create schedule B"), first);
  mutations.settle("create schedule A", 201);
  assert.notEqual(mutations.begin("create schedule A"), first);
});
