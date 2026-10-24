import { useEffect, useMemo, useState } from "react";
import type { CopyKey } from "./config";
import type { VocabularyEntry, VocabularyQuizQuestion } from "./types";
import { isQuizEligibleWord, selectQuizEntries } from "./quizSelection";

type Translator = (key: CopyKey) => string;
const COUNT_KEY = "aurora-vocabulary-quiz-count";

function initialCount(): number {
  try {
    const value = Number(localStorage.getItem(COUNT_KEY));
    return Number.isInteger(value) && value >= 1 && value <= 50 ? value : 10;
  } catch { return 10; }
}

export function VocabularyQuiz({ entries, modelReady, modelName, generateQuestion, openSettings, t }: {
  entries: VocabularyEntry[];
  modelReady: boolean;
  modelName: string;
  generateQuestion: (entryId: number) => Promise<VocabularyQuizQuestion>;
  openSettings: () => void;
  t: Translator;
}) {
  const [count, setCount] = useState(initialCount);
  const [queue, setQueue] = useState<VocabularyEntry[]>([]);
  const [phase, setPhase] = useState<"setup" | "running" | "finished">("setup");
  const [index, setIndex] = useState(0);
  const [question, setQuestion] = useState<VocabularyQuizQuestion | null>(null);
  const [choice, setChoice] = useState<number | null>(null);
  const [wrong, setWrong] = useState<VocabularyEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const eligibleCount = useMemo(() => entries.filter((entry) => isQuizEligibleWord(entry.word)).length, [entries]);
  const actualCount = Math.min(count, eligibleCount);

  useEffect(() => {
    if (phase !== "running" || question || error || loading || !queue[index]) return;
    let cancelled = false;
    setLoading(true);
    generateQuestion(queue[index].id).then((next) => {
      if (cancelled) return;
      if (next.entryId !== queue[index].id || next.options.length !== 4 || next.correctIndex < 0 || next.correctIndex > 3) {
        throw new Error(t("quizInvalidQuestion"));
      }
      setQuestion(next);
    }).catch((reason) => {
      if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
    }).finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [phase, question, error, queue, index, retry, generateQuestion, t]);

  const changeCount = (value: number) => {
    const next = Math.max(1, Math.min(50, Math.floor(Number.isFinite(value) ? value : 1)));
    setCount(next);
    try { localStorage.setItem(COUNT_KEY, String(next)); } catch { /* persistence is optional */ }
  };
  const start = () => {
    if (!modelReady || !actualCount) return;
    setQueue(selectQuizEntries(entries, count));
    setIndex(0);
    setQuestion(null);
    setChoice(null);
    setWrong([]);
    setError("");
    setPhase("running");
  };
  const answer = (selected: number) => {
    if (!question || choice !== null) return;
    setChoice(selected);
    if (selected !== question.correctIndex && queue[index]) setWrong((current) => [...current, queue[index]]);
  };
  const advance = () => {
    if (index + 1 >= queue.length) { setPhase("finished"); return; }
    setIndex((current) => current + 1);
    setQuestion(null);
    setChoice(null);
    setError("");
  };

  return <section className="vocabulary-quiz" aria-label={t("quizTitle")}>
    {phase === "setup" && <div className="vocabulary-quiz-card">
      <div className="vocabulary-quiz-heading"><h2>{t("quizTitle")}</h2><span>{modelName}</span></div>
      <p className="vocabulary-quiz-intro">{t("quizIntro")}</p>
      <label className="vocabulary-quiz-count">{t("quizQuestionCount")}<input type="number" min="1" max="50" value={count} onChange={(event) => changeCount(Number(event.target.value))} /></label>
      <p className="vocabulary-quiz-note">{actualCount} / {eligibleCount} {t("quizAvailable")}</p>
      {!modelReady && <div className="vocabulary-quiz-notice">{t("quizModelMissing")}<button type="button" onClick={openSettings}>{t("quizOpenSettings")}</button></div>}
      {modelReady && eligibleCount === 0 && <p className="vocabulary-quiz-note">{t("quizNoWords")}</p>}
      <button className="primary-button" type="button" disabled={!modelReady || !eligibleCount} onClick={start}>{t("quizStart")}</button>
    </div>}
    {phase === "running" && <div className="vocabulary-quiz-card">
      <div className="vocabulary-quiz-heading"><h2>{t("quizTitle")}</h2><span>{index + 1} / {queue.length}</span></div>
      <div className="vocabulary-quiz-progress" aria-hidden="true"><span style={{ width: `${((index + 1) / queue.length) * 100}%` }} /></div>
      {loading && <p className="vocabulary-quiz-note" role="status">{t("quizGenerating")}</p>}
      {error && <div className="vocabulary-quiz-error" role="alert"><p>{error}</p><button type="button" className="quiet-button" onClick={() => { setError(""); setRetry((value) => value + 1); }}>{t("quizRetry")}</button></div>}
      {question && <>
        <p className="vocabulary-quiz-sentence">{question.sentence}</p>
        <p className="vocabulary-quiz-translation">{question.translation}</p>
        <div className="vocabulary-quiz-options">{question.options.map((option, optionIndex) => <button key={`${option}-${optionIndex}`} type="button" disabled={choice !== null} className={`${choice !== null && optionIndex === question.correctIndex ? "is-correct" : ""} ${choice === optionIndex && choice !== question.correctIndex ? "is-wrong" : ""}`} onClick={() => answer(optionIndex)}><b>{"ABCD"[optionIndex]}</b><span>{option}</span></button>)}</div>
        {choice === null && <button type="button" className="vocabulary-quiz-regenerate" onClick={() => { setQuestion(null); setRetry((value) => value + 1); }}>{t("quizRegenerate")}</button>}
        {choice !== null && <div className="vocabulary-quiz-feedback" role="status"><strong>{choice === question.correctIndex ? t("quizCorrect") : t("quizIncorrect")}</strong><span>{t("quizCorrectAnswer")}：{question.options[question.correctIndex]}</span><p>{question.explanation}</p></div>}
        <p className="vocabulary-quiz-note">{t("quizAiNote")}</p>
        {choice !== null && <button type="button" className="primary-button" onClick={advance}>{index + 1 === queue.length ? t("quizSeeResults") : t("quizNext")}</button>}
      </>}
    </div>}
    {phase === "finished" && <div className="vocabulary-quiz-card">
      <div className="vocabulary-quiz-heading"><h2>{t("quizResults")}</h2><span>{queue.length - wrong.length} / {queue.length}</span></div>
      <p className="vocabulary-quiz-note">{wrong.length ? t("quizWrongSummary") : t("quizAllCorrect")}</p>
      {wrong.length > 0 && <div className="vocabulary-quiz-wrong">{wrong.map((entry) => <article key={entry.id}><strong>{entry.word}</strong><p>{entry.definitionMarkdown.replace(/^#+\s*/gm, "").replace(/^[-*+]\s*/gm, "").trim().slice(0, 180)}</p></article>)}</div>}
      <button className="primary-button" type="button" onClick={start} disabled={!modelReady || !eligibleCount}>{t("quizAgain")}</button>
    </div>}
  </section>;
}
