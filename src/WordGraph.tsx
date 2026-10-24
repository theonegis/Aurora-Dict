import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { CopyKey } from "./config";
import type { LocalModelId, WordGraph } from "./types";

type GraphState = { status: "loading" | "ready" | "error"; graph?: WordGraph; error?: string };

function sourceLink(url: string, label: string) {
  if (!url.startsWith("https://")) return null;
  return <button className="word-graph-source" type="button" onClick={() => void openUrl(url)}>{label} ↗</button>;
}

export function WordGraphPanel({ query, modelId, modelReady, onLookup, t }: {
  query: string;
  modelId: LocalModelId;
  modelReady: boolean;
  onLookup: (word: string) => void;
  t: (key: CopyKey) => string;
}) {
  const word = query.trim().toLowerCase();
  const eligible = /^[a-z]{2,40}$/.test(word);
  const desktop = "__TAURI_INTERNALS__" in window;
  const [retry, setRetry] = useState(0);
  const [state, setState] = useState<GraphState>({ status: "loading" });
  const [mnemonic, setMnemonic] = useState("");
  const [mnemonicPending, setMnemonicPending] = useState(false);
  const [mnemonicError, setMnemonicError] = useState("");

  useEffect(() => {
    if (!eligible || !desktop) return;
    let active = true;
    setState({ status: "loading" });
    setMnemonic("");
    setMnemonicError("");
    void invoke<WordGraph>("lookup_word_graph", { query: word }).then(
      (graph) => { if (active) setState({ status: "ready", graph }); },
      (error) => { if (active) setState({ status: "error", error: String(error) }); },
    );
    return () => { active = false; };
  }, [word, eligible, desktop, retry]);

  if (!eligible) return null;
  const graph = state.graph;
  const hasEvidence = Boolean(graph && (graph.morphology.length || graph.senses.length));
  const hasRelations = Boolean(graph && (hasEvidence || graph.spelling.length));

  const generateMnemonic = async () => {
    if (!graph || !hasEvidence) return;
    const evidence = [
      ...graph.morphology.map((part) => `构词：${part.label}`),
      ...graph.senses.map((sense) => `义项 ${sense.id} (${sense.gloss})：${sense.synonyms.map((item) => item.word).join("、")}`),
    ].join("\n");
    setMnemonicPending(true);
    setMnemonicError("");
    try {
      setMnemonic(await invoke<string>("generate_graph_mnemonic", { modelId, word, evidence }));
    } catch (error) {
      setMnemonicError(String(error));
    } finally {
      setMnemonicPending(false);
    }
  };

  return <section className="word-graph-card" aria-labelledby="word-graph-title">
    <header className="word-graph-header"><h2 id="word-graph-title">{t("wordGraphTitle")}</h2></header>
    {!desktop && <p className="word-graph-message">{t("wordGraphUnavailable")}</p>}
    {desktop && state.status === "loading" && <p className="word-graph-message" role="status">{t("wordGraphLoading")}</p>}
    {desktop && state.status === "error" && <div className="word-graph-message"><p>{state.error}</p><button type="button" className="word-graph-retry" onClick={() => setRetry((value) => value + 1)}>{t("wordGraphRetry")}</button></div>}
    {graph && <>
      <div className="word-graph-canvas">
        <div className="word-graph-center"><span>{graph.word}</span></div>
        {hasRelations ? <div className="word-graph-branches">
          <div className="word-graph-branch"><h3>{t("wordGraphMorphology")}</h3>
            {graph.morphology.length ? graph.morphology.map((part) => <div className="word-graph-item" key={part.label}>
              <div className="word-graph-components">{part.components.map((component, index) => {
                const target = component.replace(/^-|-$/g, "");
                return /^[a-z]{2,40}$/i.test(target) && !component.startsWith("-") && !component.endsWith("-")
                  ? <button type="button" onClick={() => onLookup(target)} key={`${component}-${index}`}>{component}</button>
                  : <span key={`${component}-${index}`}>{component}</span>;
              })}</div>
              {sourceLink(part.sourceUrl, t("wordGraphSource"))}
            </div>) : <p className="word-graph-none">—</p>}
          </div>
          <div className="word-graph-branch"><h3>{t("wordGraphMeaning")}</h3>
            {graph.senses.length ? graph.senses.map((sense) => <div className="word-graph-item" key={sense.id}>
              <p className="word-graph-gloss">{sense.gloss} <small>{sense.partOfSpeech}</small></p>
              <div className="word-graph-words">{sense.synonyms.map((item) => <button type="button" key={item.word} title={sense.gloss} onClick={() => onLookup(item.word)}>{item.word}</button>)}</div>
              {sourceLink(sense.synonyms[0].sourceUrl, t("wordGraphSource"))}
            </div>) : <p className="word-graph-none">—</p>}
          </div>
          <div className="word-graph-branch"><h3>{t("wordGraphSpelling")}</h3>
            {graph.spelling.length ? <div className="word-graph-words">{graph.spelling.map((item) => <button type="button" key={item.word} title={item.detail} onClick={() => onLookup(item.word)}>{item.word}</button>)}</div> : <p className="word-graph-none">—</p>}
          </div>
        </div> : <p className="word-graph-message">{t("wordGraphEmpty")}</p>}
      </div>
      {graph.warnings.length > 0 && <p className="word-graph-warning">{graph.warnings.join(" · ")}</p>}
      <div className="word-graph-attribution"><button type="button" onClick={() => void openUrl("https://en-word.net/")}>Open English WordNet</button><span>·</span><button type="button" onClick={() => void openUrl("https://kaikki.org/dictionary/English/")}>Kaikki / Wiktionary</button></div>
      {hasEvidence && modelReady && <div className="word-graph-ai"><button type="button" className="word-graph-retry" disabled={mnemonicPending} onClick={() => void generateMnemonic()}>{mnemonicPending ? t("wordGraphLoading") : t("wordGraphMnemonic")}</button>
        {mnemonic && <p>{mnemonic}<small>{t("wordGraphMnemonicNote")}</small></p>}
        {mnemonicError && <p className="word-graph-warning">{mnemonicError}</p>}
      </div>}
    </>}
  </section>;
}
