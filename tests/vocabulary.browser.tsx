import React, { useCallback, useState } from "react";
import { createRoot } from "react-dom/client";
import { VocabularyPanel } from "../src/VocabularyPanel";
import { VocabularyQuiz } from "../src/VocabularyQuiz";
import type { VocabularyEntry } from "../src/types";
import "../src/style.css";

const initialEntries: VocabularyEntry[] = [
  { id: 1, word: "serendipity", phonetic: "/ˌserənˈdɪpəti/", definitionMarkdown: "## 释义\n\n- 偶然发现珍宝的运气", examplesMarkdown: "", createdAt: 1, updatedAt: 1 },
  { id: 2, word: "kindness", phonetic: "/ˈkaɪndnəs/", definitionMarkdown: "## 释义\n\n- 仁慈", examplesMarkdown: "", createdAt: 2, updatedAt: 2 },
];

declare global {
  interface Window { __vocabularyTest: { removed: number[]; generated: number[]; settingsOpened: boolean } }
}
window.__vocabularyTest = { removed: [], generated: [], settingsOpened: false };

function Fixture() {
  const [panel, setPanel] = useState<"book" | "quiz">("book");
  const [entries, setEntries] = useState(new URLSearchParams(window.location.search).has("empty") ? [] : initialEntries);
  const generateQuestion = useCallback(async (entryId: number) => {
    window.__vocabularyTest.generated.push(entryId);
    if (new URLSearchParams(window.location.search).has("fail") && window.__vocabularyTest.generated.length === 1) throw new Error("生成失败");
    const word = initialEntries.find((entry) => entry.id === entryId)?.word ?? "kindness";
    return { entryId, word, sentence: "She showed ____ today.", translation: "她今天表现出了这一品质。", explanation: "句意对应这个词的含义。", options: ["anger", word, "care", "hope"] as [string, string, string, string], correctIndex: 1 };
  }, []);
  const t = (key: string) => ({ vocabularyEdit: "编辑", vocabularyDelete: "删除", vocabularyConfirmDelete: "确认删除", vocabularySummary: "生词汇总", vocabularyDetails: "单词详解", vocabularyBook: "生词本", quizTitle: "背单词", quizStart: "开始练习", quizNext: "下一题", quizSeeResults: "查看结果", quizResults: "练习结果", quizRetry: "重新生成", quizOpenSettings: "前往设置", quizCorrect: "回答正确", quizIncorrect: "回答错误", quizCorrectAnswer: "正确答案", cancel: "取消" } as Record<string, string>)[key] ?? key;
  return <div className="app-shell">
    <nav className="quick-actions" aria-label="主要导航">
      <button className={`quick-action${panel === "book" ? " is-active" : ""}`} type="button" aria-current={panel === "book" ? "page" : undefined} onClick={() => setPanel("book")}>生词本</button>
      <button className={`quick-action${panel === "quiz" ? " is-active" : ""}`} type="button" aria-current={panel === "quiz" ? "page" : undefined} onClick={() => setPanel("quiz")}>背单词</button>
    </nav>
    <div hidden={panel !== "book"}><VocabularyPanel
      entries={entries} status={{ path: "test.sqlite3", entryCount: entries.length }} loading={false} error="" isDesktop={false}
      persist={async (input) => {
        const entry: VocabularyEntry = { ...input, id: input.id ?? 3, createdAt: 3, updatedAt: 3 };
        setEntries((current) => current.map((item) => item.id === entry.id ? entry : item));
        return entry;
      }}
      remove={async (id) => { window.__vocabularyTest.removed.push(id); setEntries((current) => current.filter((item) => item.id !== id)); }}
      refresh={async () => {}} importBook={async () => ({ path: "", entryCount: 0 })}
      exportBook={async () => ({ path: "", entryCount: 0 })}
      moveBook={async () => ({ path: "", entryCount: 0 })}
      openBook={async () => ({ path: "", entryCount: 0 })}
      t={t}
    /></div>
    <div hidden={panel !== "quiz"}><VocabularyQuiz entries={entries} modelReady={!new URLSearchParams(window.location.search).has("modelMissing")} modelName="Qwen3-0.6B" generateQuestion={generateQuestion} openSettings={() => { window.__vocabularyTest.settingsOpened = true; }} t={t} /></div>
  </div>;
}

createRoot(document.getElementById("root")!).render(<Fixture />);
