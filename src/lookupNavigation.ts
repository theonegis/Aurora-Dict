import type { SourceId } from "./types";

export interface LookupLocation {
  query: string;
  source: SourceId;
}

export function pushGraphLookup(history: readonly LookupLocation[], current: LookupLocation, target: string): readonly LookupLocation[] {
  const nextWord = target.trim();
  const currentWord = current.query.trim();
  if (!nextWord || !currentWord || nextWord.toLocaleLowerCase() === currentWord.toLocaleLowerCase()) return history;
  return [...history, { query: currentWord, source: current.source }];
}

export function takePreviousLookup(history: readonly LookupLocation[]): { previous: LookupLocation | null; history: readonly LookupLocation[] } {
  return history.length
    ? { previous: history[history.length - 1], history: history.slice(0, -1) }
    : { previous: null, history };
}
