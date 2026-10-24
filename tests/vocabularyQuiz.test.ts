import assert from "node:assert/strict";
import test from "node:test";
import { isQuizEligibleWord, selectQuizEntries } from "../src/quizSelection.ts";
import type { VocabularyEntry } from "../src/types.ts";

const entry = (id: number, word: string, createdAt: number): VocabularyEntry => ({
  id, word, createdAt, updatedAt: createdAt, phonetic: "", definitionMarkdown: "", examplesMarkdown: "",
});

test("quiz selects single English words without repeats and caps the count", () => {
  const selected = selectQuizEntries([
    entry(1, "serendipity", 1), entry(2, "take off", 2), entry(3, "中文", 3), entry(4, "splendid", 4),
  ], 50, () => 0);
  assert.deepEqual(selected.map(({ word }) => word), ["splendid", "serendipity"]);
  assert.equal(isQuizEligibleWord("don't"), true);
  assert.equal(isQuizEligibleWord("well-known"), false);
});

test("newer words receive higher weighted selection probability", () => {
  const entries = [entry(1, "oldest", 1), entry(2, "middle", 2), entry(3, "newest", 3)];
  const draws = Array.from({ length: 100 }, (_, i) => selectQuizEntries(entries, 1, () => i / 100)[0].word);
  assert.ok(draws.filter((word) => word === "newest").length > draws.filter((word) => word === "oldest").length);
});
