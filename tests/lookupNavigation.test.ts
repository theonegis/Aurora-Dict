import assert from "node:assert/strict";
import test from "node:test";
import { pushGraphLookup, takePreviousLookup } from "../src/lookupNavigation.ts";

test("a graph link remembers the word and source it came from", () => {
  const history = pushGraphLookup([], { query: "kindness", source: "local" }, "kind");
  assert.deepEqual(history, [{ query: "kindness", source: "local" }]);
});

test("returning through multiple graph links restores each previous word in order", () => {
  const first = pushGraphLookup([], { query: "kindness", source: "local" }, "kind");
  const second = pushGraphLookup(first, { query: "kind", source: "youdao" }, "kinds");
  assert.deepEqual(takePreviousLookup(second), {
    previous: { query: "kind", source: "youdao" },
    history: [{ query: "kindness", source: "local" }],
  });
  assert.deepEqual(takePreviousLookup(first), {
    previous: { query: "kindness", source: "local" },
    history: [],
  });
});

test("clicking the current word does not create a false back step", () => {
  const history = [{ query: "origin", source: "local" as const }];
  assert.equal(pushGraphLookup(history, { query: "Kindness", source: "local" }, " kindness "), history);
});

test("an empty graph target is ignored and an empty history has nowhere to return", () => {
  const history = [{ query: "origin", source: "local" as const }];
  assert.equal(pushGraphLookup(history, { query: "kindness", source: "local" }, " "), history);
  assert.deepEqual(takePreviousLookup([]), { previous: null, history: [] });
});
