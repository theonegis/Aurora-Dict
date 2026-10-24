import type { VocabularyEntry } from "./types";

export function isQuizEligibleWord(word: string): boolean {
  return /^[A-Za-z]+(?:'[A-Za-z]+)?$/.test(word.trim());
}

export function selectQuizEntries(entries: VocabularyEntry[], count: number, random: () => number = Math.random): VocabularyEntry[] {
  const pool = entries.filter((entry) => isQuizEligibleWord(entry.word))
    .sort((a, b) => b.createdAt - a.createdAt || b.id - a.id)
    .map((entry, index, eligible) => ({ entry, weight: eligible.length <= 1 ? 5 : 5 - (4 * index) / (eligible.length - 1) }));
  const selected: VocabularyEntry[] = [];
  const limit = Math.min(pool.length, Math.max(1, Math.min(50, Math.floor(Number.isFinite(count) ? count : 10))));
  while (selected.length < limit) {
    const total = pool.reduce((sum, candidate) => sum + candidate.weight, 0);
    let draw = Math.min(0.999999999, Math.max(0, random())) * total;
    let choice = pool.length - 1;
    for (let i = 0; i < pool.length; i += 1) {
      draw -= pool[i].weight;
      if (draw < 0) { choice = i; break; }
    }
    selected.push(pool.splice(choice, 1)[0].entry);
  }
  return selected;
}
