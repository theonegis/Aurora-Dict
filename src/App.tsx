import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, FormEvent, MouseEvent as ReactMouseEvent, PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { readText as readClipboardText, writeText as writeClipboardText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getSystemFonts } from "tauri-plugin-system-fonts-api";
import { Icon } from "./Icon";
import { pushGraphLookup, takePreviousLookup } from "./lookupNavigation";
import type { LookupLocation } from "./lookupNavigation";
import { VocabularyPanel } from "./VocabularyPanel";
import { VocabularyQuiz } from "./VocabularyQuiz";
import { WordGraphPanel } from "./WordGraph";
import { PronunciationRow } from "./Pronunciation";
import {
  CACHE_LIMIT_OPTIONS,
  DEFAULT_DICTIONARY_SYSTEM_PROMPT,
  DEFAULT_TRANSLATION_SYSTEM_PROMPT,
  MAX_ENABLED_SOURCES,
  SYSTEM_FONT_ID,
  copy,
  fallbackLookup,
  initialFonts,
  localModels,
  sources,
  systemFontStacks,
  themes,
} from "./config";
import type { CopyKey } from "./config";
import { cacheSourceResults, cloneDefaultSettings, loadSettings, normaliseCacheLimit, persistSettings, readCachedQuery, trimQueryCache } from "./storage";
import type {
  AppState,
  DictionaryEntry,
  DictionaryStatus,
  DisplaySettings,
  DownloadProgress,
  FontId,
  LlmLookup,
  LlmModelStatus,
  LlmPerformance,
  LlmPrepareResult,
  LlmStatus,
  LlmStreamUpdate,
  LlmTranslation,
  LocalModelId,
  LocalSuggestions,
  OnlineContentSection,
  OnlineExample,
  OnlineLookup,
  OnlinePhrase,
  OnlineSense,
  PanelId,
  SettingsTabId,
  SourceId,
  SourceLookupResult,
  VocabularyBookStatus,
  VocabularyEntry,
  VocabularyEntryInput,
  VocabularyQuizQuestion,
} from "./types";

const appIconUrl = new URL("../src-tauri/icons/icon.png", import.meta.url).href;
const wechatDonationUrl = new URL("../src-tauri/resources/IMG_2816.JPG", import.meta.url).href;
const alipayDonationUrl = new URL("../src-tauri/resources/IMG_2817.JPG", import.meta.url).href;
const homepageUrl = "https://theonegis.github.io";

function isTauri(): boolean { return "__TAURI_INTERNALS__" in window; }
function isMac(): boolean { return /Mac|iPhone|iPad|iPod/.test(navigator.platform); }
function isWindows(): boolean { return /Win/.test(navigator.platform); }
function platformName(): keyof typeof systemFontStacks {
  if (["localhost", "127.0.0.1", "[::1]"].includes(window.location.hostname)) {
    const preview = new URLSearchParams(window.location.search).get("platform");
    if (preview === "windows" || preview === "macos" || preview === "linux") return preview;
  }
  return isWindows() ? "windows" : isMac() ? "macos" : "linux";
}
function isEnglishInput(value: string): boolean { return /^[a-z][a-z' -]*$/i.test(value.trim()); }
function clamp(value: number, minimum: number, maximum: number): number { return Math.min(Math.max(value, minimum), maximum); }

function openExternalUrl(url: string): void {
  if (isTauri()) {
    void openUrl(url);
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

function promptFingerprint(prompt: string, fallback: string): string {
  const content = prompt.trim() || fallback;
  let hash = 2_166_136_261;
  for (let index = 0; index < content.length; index += 1) {
    hash ^= content.charCodeAt(index);
    hash = Math.imul(hash, 16_777_619);
  }
  // Invalidate AI answers cached before the Intel Mac inference fix.
  return `inference-v2:${(hash >>> 0).toString(16).padStart(8, "0")}`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1_024).toFixed(1)} KB`;
  if (bytes < 1_073_741_824) return `${(bytes / 1_048_576).toFixed(1)} MB`;
  return `${(bytes / 1_073_741_824).toFixed(2)} GB`;
}

function splitDefinitions(value: string): string[] {
  return value.split(/[；;\n]/).map((definition) => definition.trim()).filter(Boolean).slice(0, 8);
}

function splitSenses(entry: DictionaryEntry, t: Translator): Array<{ label: string; definitions: string[] }> {
  const translation = entry.translation?.trim() || entry.definition?.trim() || t("noDefinition");
  const partPattern = /(?:^|[；;\n])\s*((?:(?:n|v|vt|vi|adj|adv|prep|pron|conj|art|aux|int|num|phr)\.)+)/gi;
  const matches = [...translation.matchAll(partPattern)];
  if (!matches.length) return [{ label: entry.pos?.trim() || t("definition"), definitions: splitDefinitions(translation) }];
  return matches.map((match, index) => {
    const start = (match.index ?? 0) + match[0].length;
    const end = index + 1 < matches.length ? matches[index + 1].index ?? translation.length : translation.length;
    return { label: match[1], definitions: splitDefinitions(translation.slice(start, end)) };
  });
}

function parseWordForms(exchange: string | undefined): Array<{ code: string; value: string }> {
  if (!exchange) return [];
  return exchange.split("/").map((item) => {
    const separator = item.indexOf(":");
    if (separator <= 0) return null;
    const code = item.slice(0, separator).trim();
    const value = item.slice(separator + 1).trim();
    return code && value ? { code, value } : null;
  }).filter((item): item is { code: string; value: string } => item !== null);
}

type Translator = (key: CopyKey) => string;

function formatText(t: Translator, key: CopyKey, values: Record<string, string>): string {
  return Object.entries(values).reduce((text, [name, value]) => text.replaceAll(`{${name}}`, value), t(key));
}

function llmRequestId(operation: "lookup" | "translation"): string {
  return `${operation}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function formatDuration(milliseconds: number): string {
  if (milliseconds < 1_000) return `${Math.max(0, Math.round(milliseconds))} ms`;
  return `${(milliseconds / 1_000).toFixed(milliseconds < 10_000 ? 2 : 1)} s`;
}

function LlmPerformanceLine({ performance, t }: { performance?: LlmPerformance; t: Translator }) {
  if (!performance) return null;
  const items = [
    performance.coldStart && performance.startupMs > 0 ? `${t("llmColdStart")} ${formatDuration(performance.startupMs)}` : "",
    `${t("llmFirstToken")} ${formatDuration(performance.firstTokenMs)}`,
    `${t("llmTotalTime")} ${formatDuration(performance.totalMs)}`,
    performance.tokensPerSecond ? `${performance.tokensPerSecond.toFixed(1)} token/s` : "",
  ].filter(Boolean);
  return <div className="llm-performance"><i className="fa-solid fa-gauge-high" aria-hidden="true" /><span>{items.join(" · ")}</span></div>;
}

interface SelectionMenuState {
  text: string;
  lookupText: string;
  input: HTMLInputElement | HTMLTextAreaElement | null;
  selectionStart: number;
  selectionEnd: number;
  x: number;
  y: number;
}

function normaliseLookupSelection(value: string): string {
  const text = value.replace(/\s+/g, " ").trim().replace(/^[“”‘’"'.,，。！？!?;；:：()（）[\]{}<>《》【】]+|[“”‘’"'.,，。！？!?;；:：()（）[\]{}<>《》【】]+$/g, "").trim();
  return text.length > 0 && text.length <= 120 ? text : "";
}

function selectionContextInSurface(target: EventTarget | null, surface: HTMLElement): Pick<SelectionMenuState, "text" | "lookupText" | "input" | "selectionStart" | "selectionEnd"> {
  if (target instanceof HTMLTextAreaElement || (target instanceof HTMLInputElement && ["search", "text"].includes(target.type))) {
    const start = target.selectionStart ?? 0;
    const end = target.selectionEnd ?? 0;
    const text = start === end ? "" : target.value.slice(start, end);
    return { text, lookupText: normaliseLookupSelection(text), input: target, selectionStart: start, selectionEnd: end };
  }
  const empty = { text: "", lookupText: "", input: null, selectionStart: 0, selectionEnd: 0 };
  if (target instanceof Element && target.closest("button, select, option")) return empty;
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || !selection.anchorNode || !selection.focusNode || !surface.contains(selection.anchorNode) || !surface.contains(selection.focusNode)) return empty;
  const text = selection.toString();
  return { ...empty, text, lookupText: normaliseLookupSelection(text) };
}

async function copyTextToClipboard(text: string): Promise<void> {
  if (isTauri()) {
    await writeClipboardText(text);
    return;
  }
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const fallback = document.createElement("textarea");
  fallback.value = text;
  fallback.style.position = "fixed";
  fallback.style.opacity = "0";
  document.body.append(fallback);
  fallback.select();
  document.execCommand("copy");
  fallback.remove();
}

async function pasteTextFromClipboard(): Promise<string> {
  if (isTauri()) return readClipboardText();
  return navigator.clipboard?.readText ? navigator.clipboard.readText() : "";
}

function SelectionLookupMenu({ menu, copySelection, lookup, addVocabulary, paste, t }: { menu: SelectionMenuState; copySelection: (text: string) => void; lookup: (text: string) => void; addVocabulary: (text: string) => void; paste: (menu: SelectionMenuState) => void; t: Translator }) {
  return <div className="selection-lookup-menu" role="menu" style={{ left: menu.x, top: menu.y }} onContextMenu={(event) => event.preventDefault()}>
    {menu.text && <button type="button" role="menuitem" autoFocus onClick={() => copySelection(menu.text)}><i className="fa-solid fa-copy" aria-hidden="true" /><span>{t("contextCopy")}</span></button>}
    {menu.lookupText && <button type="button" role="menuitem" autoFocus={!menu.text} onClick={() => lookup(menu.lookupText)}><i className="fa-solid fa-magnifying-glass" aria-hidden="true" /><span>{t("contextQuery")}</span></button>}
    {menu.lookupText && <button type="button" role="menuitem" onClick={() => addVocabulary(menu.lookupText)}><i className="fa-solid fa-book-medical" aria-hidden="true" /><span>{t("contextAddVocabulary")}</span></button>}
    {menu.input && <button type="button" role="menuitem" autoFocus={!menu.text && !menu.lookupText} onClick={() => paste(menu)}><i className="fa-solid fa-paste" aria-hidden="true" /><span>{t("contextPaste")}</span></button>}
  </div>;
}

function displayHeadword(word: string, query: string): string {
  const dictionaryWord = word.trim();
  const submittedQuery = query.trim();
  return submittedQuery && submittedQuery.toLocaleLowerCase() === dictionaryWord.toLocaleLowerCase() ? submittedQuery : dictionaryWord;
}

function WindowControls({ t }: { t: Translator }) {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    let stopListening: (() => void) | undefined;
    const syncMaximized = () => {
      void appWindow.isMaximized().then((value) => {
        if (!disposed) setMaximized(value);
      }).catch(() => undefined);
    };
    syncMaximized();
    void appWindow.onResized(syncMaximized).then((unlisten) => {
      if (disposed) unlisten();
      else stopListening = unlisten;
    }).catch(() => undefined);
    return () => {
      disposed = true;
      stopListening?.();
    };
  }, []);

  const controlWindow = async (action: "close" | "minimise" | "maximise") => {
    if (!isTauri()) return;
    const appWindow = getCurrentWindow();
    if (action === "close") await appWindow.close();
    if (action === "minimise") await appWindow.minimize();
    if (action === "maximise") {
      await appWindow.toggleMaximize();
      setMaximized(await appWindow.isMaximized());
    }
  };
  return <div className="win-controls" aria-label={t("titlebarCaption")}>
    <button onClick={() => void controlWindow("minimise")} aria-label={t("minimiseWindow")}><Icon name="windowMinimize" size={12} /></button>
    <button onClick={() => void controlWindow("maximise")} aria-label={t("maximiseWindow")}><Icon name={maximized ? "restore" : "maximize"} size={12} /></button>
    <button className="win-close" onClick={() => void controlWindow("close")} aria-label={t("closeWindow")}><Icon name="windowClose" size={12} /></button>
  </div>;
}

function TitleBar({ t }: { t: Translator }) {
  const startDragging = (event: ReactPointerEvent<HTMLElement>) => {
    if (event.button !== 0 || !isTauri()) return;
    const target = event.target;
    if (target instanceof Element && target.closest("button, input, select, a")) return;
    event.preventDefault();
    void getCurrentWindow().startDragging().catch(() => undefined);
  };
  return <header className="titlebar">
    <div className="titlebar-drag-area" data-tauri-drag-region onPointerDown={startDragging}>
      <div className="brand"><img className="brand-app-icon" src={appIconUrl} alt="" aria-hidden="true" /><span>Aurora <em>Dict</em></span></div>
      <div className="titlebar-caption">{t("titlebarCaption")}</div>
    </div>
    {!isMac() && <WindowControls t={t} />}
  </header>;
}

export function Hero({ panel, setPanel, previousWord, returnToPrevious, t }: { panel: PanelId; setPanel: (panel: PanelId) => void; previousWord: string | null; returnToPrevious: () => void; t: Translator }) {
  const panelTitle = panel === "dictionary" ? t("quickDictionaryTip") : panel === "translation" ? t("quickTranslationTip") : panel === "vocabulary" ? t("quickVocabularyTip") : panel === "quiz" ? t("quizTitle") : t("quickSettingsTip");
  return <section className={`hero${panel === "dictionary" ? " is-dictionary" : ""}`}>
    <div className="hero-heading"><h1>{panelTitle}</h1>{panel === "dictionary" && previousWord && <button className="lookup-return" type="button" onClick={returnToPrevious} aria-label={formatText(t, "wordGraphBack", { word: previousWord })} title={formatText(t, "wordGraphBack", { word: previousWord })}><i className="fa-solid fa-arrow-left" aria-hidden="true" /><span>{formatText(t, "wordGraphBack", { word: previousWord })}</span></button>}</div>
    <nav className="quick-actions" aria-label={t("mainNavigation")}>
      <button className={`quick-action ${panel === "dictionary" ? "is-active" : ""}`} onClick={() => setPanel("dictionary")} type="button" aria-current={panel === "dictionary" ? "page" : undefined} aria-label={t("quickDictionaryTip")} title={t("quickDictionaryTip")}><i className="fa-solid fa-magnifying-glass" aria-hidden="true" /><span>{t("quickDictionaryTip")}</span></button>
      <button className={`quick-action ${panel === "translation" ? "is-active" : ""}`} onClick={() => setPanel("translation")} type="button" aria-current={panel === "translation" ? "page" : undefined} aria-label={t("quickTranslationTip")} title={t("quickTranslationTip")}><i className="fa-solid fa-language" aria-hidden="true" /><span>{t("quickTranslationTip")}</span></button>
      <button className={`quick-action ${panel === "vocabulary" ? "is-active" : ""}`} onClick={() => setPanel("vocabulary")} type="button" aria-current={panel === "vocabulary" ? "page" : undefined} aria-label={t("quickVocabularyTip")} title={t("quickVocabularyTip")}><i className="fa-solid fa-book-bookmark" aria-hidden="true" /><span>{t("quickVocabularyTip")}</span></button>
      <button className={`quick-action ${panel === "quiz" ? "is-active" : ""}`} onClick={() => setPanel("quiz")} type="button" aria-current={panel === "quiz" ? "page" : undefined} aria-label={t("quizTitle")} title={t("quizTitle")}><i className="fa-solid fa-graduation-cap" aria-hidden="true" /><span>{t("quizTitle")}</span></button>
      <button className={`quick-action ${panel === "settings" ? "is-active" : ""}`} onClick={() => setPanel("settings")} type="button" aria-current={panel === "settings" ? "page" : undefined} aria-label={t("quickSettingsTip")} title={t("quickSettingsTip")}><i className="fa-solid fa-sliders" aria-hidden="true" /><span>{t("quickSettingsTip")}</span></button>
    </nav>
  </section>;
}

function ContentToggle({ contentKey, expanded, toggle, t }: { contentKey: string; expanded: boolean; toggle: (key: string) => void; t: Translator }) {
  return <button className="show-more-button" type="button" onClick={() => toggle(contentKey)}>{expanded ? t("showLess") : t("showMore")}</button>;
}

function SenseCard({ label, definitions, contentKey, expandedContent, toggleExpanded, t }: {
  label: string; definitions: string[]; contentKey?: string; expandedContent: Set<string>; toggleExpanded: (key: string) => void; t: Translator;
}) {
  const expanded = Boolean(contentKey && expandedContent.has(contentKey));
  const visible = contentKey && !expanded ? definitions.slice(0, 5) : definitions;
  return <article className="sense-card"><header><span>{label}</span><i /></header><div className="definition-stack">
    {visible.map((definition, index) => <div className="definition-pill" key={`${definition}-${index}`}><span className="definition-number">{String(index + 1).padStart(2, "0")}</span><p>{definition}</p></div>)}
  </div>{contentKey && definitions.length > 5 && <ContentToggle contentKey={contentKey} expanded={expanded} toggle={toggleExpanded} t={t} />}</article>;
}

function WordForms({ exchange, t }: { exchange?: string; t: Translator }) {
  const forms = parseWordForms(exchange);
  if (!forms.length) return null;
  const descriptions: Record<string, CopyKey> = { p: "formPastTense", d: "formPastParticiple", i: "formPresentParticiple", "3": "formThirdPerson", r: "formComparative", t: "formSuperlative", s: "formPlural", "0": "formLemma", "1": "formLemmaForm" };
  return <div className="word-forms" aria-label={t("wordForms")}><span className="word-forms-label">{t("wordForms")}</span><div className="word-forms-list">
    {forms.map((form, index) => <span className="word-form-chip" title={t(descriptions[form.code] ?? "formUnknown")} key={`${form.code}-${form.value}-${index}`}><b>[{form.code}]</b><span>{form.value}</span></span>)}
  </div></div>;
}

function VocabularyAddButton({ added, add, t }: { added: boolean; add: () => void; t: Translator }) {
  return <button className={`vocabulary-add-button ${added ? "is-added" : ""}`} type="button" disabled={added} onClick={add}><i className={`fa-solid ${added ? "fa-bookmark" : "fa-book-medical"}`} aria-hidden="true" /><span>{added ? t("vocabularyAdded") : t("vocabularyAdd")}</span></button>;
}

function localVocabularyDraft(entry: DictionaryEntry, t: Translator): VocabularyEntryInput {
  const groups = splitSenses(entry, t);
  const sections = groups.map((group) => `### ${group.label}\n\n${group.definitions.map((definition) => `- ${definition}`).join("\n")}`);
  if (entry.definition?.trim() && entry.definition.trim() !== entry.translation?.trim()) sections.push(`### EN\n\n${entry.definition.trim()}`);
  return {
    id: null,
    word: entry.word,
    phonetic: entry.phonetic || entry.usPhonetic || entry.ukPhonetic || "",
    definitionMarkdown: `## ${t("definition")}\n\n${sections.join("\n\n")}`,
    examplesMarkdown: "",
  };
}

function onlineVocabularyDraft(result: OnlineLookup, senses: OnlineSense[], examples: OnlineExample[], t: Translator): VocabularyEntryInput {
  const definitions = senses.map((sense) => `### ${sense.partOfSpeech}\n\n${sense.definitions.map((definition) => `- ${definition}`).join("\n")}`).join("\n\n");
  const exampleText = examples.map((example, index) => `${index + 1}. **${example.english}**${example.translation ? `\n   ${example.translation}` : ""}`).join("\n\n");
  return {
    id: null,
    word: result.word,
    phonetic: result.pronunciation || result.usPhonetic || result.ukPhonetic || "",
    definitionMarkdown: `## ${t("definition")}\n\n${definitions}`,
    examplesMarkdown: exampleText ? `## ${t("examples")}\n\n${exampleText}` : "",
  };
}

function EntryCard({ entry, index, query, expandedContent, toggleExpanded, addVocabulary, added, t }: { entry: DictionaryEntry; index: number; query: string; expandedContent: Set<string>; toggleExpanded: (key: string) => void; addVocabulary: (entry: VocabularyEntryInput) => void; added: boolean; t: Translator }) {
  const groups = splitSenses(entry, t);
  const englishDefinition = entry.definition?.trim();
  return <section className="entry-card" style={{ "--entry-index": index } as CSSProperties}><div className="entry-card-head"><div><div className="word-line"><h2>{displayHeadword(entry.word, query)}</h2><PronunciationRow word={entry.word} fallbackPhonetic={entry.phonetic} ukPhonetic={entry.ukPhonetic} usPhonetic={entry.usPhonetic} ukAudio={entry.ukAudio} usAudio={entry.usAudio} t={t} /></div>
    <WordForms exchange={entry.exchange} t={t} />
  </div><div className="entry-card-tools"><span className="local-mark">{t("localMark")}</span><VocabularyAddButton added={added} add={() => addVocabulary(localVocabularyDraft(entry, t))} t={t} /></div></div><div className="sense-grid">{groups.map((group, groupIndex) => <SenseCard key={`${group.label}-${groupIndex}`} {...group} expandedContent={expandedContent} toggleExpanded={toggleExpanded} t={t} />)}</div>
  {englishDefinition && englishDefinition !== entry.translation && <div className="english-gloss"><span>EN</span><p>{englishDefinition}</p></div>}</section>;
}

function ExampleSection({ examples, contentKey, expandedContent, toggleExpanded, t }: { examples: OnlineExample[]; contentKey?: string; expandedContent: Set<string>; toggleExpanded: (key: string) => void; t: Translator }) {
  if (!examples.length) return null;
  const expanded = Boolean(contentKey && expandedContent.has(contentKey));
  const visible = contentKey && !expanded ? examples.slice(0, 3) : examples;
  return <section className="examples-section" aria-label={t("examples")}><header className="examples-heading"><span>{t("examples")}</span><i /><small>EXAMPLES</small></header><div className="example-stack">
    {visible.map((example, index) => <article className="example-card" key={`${example.english}-${index}`}><span className="example-number">{String(index + 1).padStart(2, "0")}</span><div><p className="example-english">{example.english}</p>{example.translation && <p className="example-translation">{example.translation}</p>}{example.source && <small className="example-source">{example.source}</small>}</div></article>)}
  </div>{contentKey && examples.length > 3 && <ContentToggle contentKey={contentKey} expanded={expanded} toggle={toggleExpanded} t={t} />}</section>;
}

function PhraseSection({ phrases, contentKey, expandedContent, toggleExpanded, t }: { phrases: OnlinePhrase[]; contentKey: string; expandedContent: Set<string>; toggleExpanded: (key: string) => void; t: Translator }) {
  if (!phrases.length) return null;
  const expanded = expandedContent.has(contentKey);
  const visible = expanded ? phrases : phrases.slice(0, 4);
  return <section className="phrases-section" aria-label={t("phrases")}><header className="examples-heading"><span>{t("phrases")}</span><i /><small>PHRASES</small></header><div className="phrase-stack">
    {visible.map((phrase, index) => <article className="phrase-card" key={`${phrase.term}-${index}`}><span className="example-number">{String(index + 1).padStart(2, "0")}</span><div><b>{phrase.term}</b>{phrase.translation && <p>{phrase.translation}</p>}</div></article>)}
  </div>{phrases.length > 4 && <ContentToggle contentKey={contentKey} expanded={expanded} toggle={toggleExpanded} t={t} />}</section>;
}

function OnlineCard({ result, query, source, youdaoSection, setYoudaoSection, expandedContent, toggleExpanded, addVocabulary, added, t }: {
  result: OnlineLookup; query: string; source: SourceId; youdaoSection: string; setYoudaoSection: (section: string) => void; expandedContent: Set<string>; toggleExpanded: (key: string) => void; addVocabulary: (entry: VocabularyEntryInput) => void; added: boolean; t: Translator;
}) {
  const activeSection: OnlineContentSection | null = source === "youdao" && result.sections.length
    ? result.sections.find((section) => section.id === youdaoSection) ?? result.sections[0]
    : null;
  const senses = activeSection?.senses ?? result.senses;
  const examples = activeSection?.examples ?? result.examples;
  const sectionId = activeSection?.id;
  const displaySource = result.source === "有道词典" ? t("youdaoSourceTitle") : result.source;
  const note = result.source === "有道词典" ? t("youdaoNote") : result.source === "FreeDictionaryAPI.com" ? t("dictionaryNote") : result.source === "Merriam-Webster" ? t("merriamWebsterNote") : t("genericOnlineNote");
  return <section className="entry-card online-entry"><div className="entry-card-head"><div><div className="word-line"><h2>{displayHeadword(result.word, query)}</h2><PronunciationRow word={result.word} fallbackPhonetic={result.pronunciation} ukPhonetic={result.ukPhonetic} usPhonetic={result.usPhonetic} ukAudio={result.ukAudio} usAudio={result.usAudio} stripOuterSlashes t={t} /></div>
    <p className="source-credit">{formatText(t, "structuredSource", { source: displaySource })}</p></div><div className="entry-card-tools"><span className="online-mark">{t("onlineMark")}</span><VocabularyAddButton added={added} add={() => addVocabulary(onlineVocabularyDraft(result, senses, examples, t))} t={t} /></div></div>
    {result.sections.length > 0 && source === "youdao" && <div className="youdao-tabs" role="tablist" aria-label={result.source}>{result.sections.map((section) => <button type="button" role="tab" key={section.id} aria-selected={section.id === sectionId} className={`youdao-tab ${section.id === sectionId ? "is-active" : ""}`} onClick={() => setYoudaoSection(section.id)}>{section.id === "collins" ? t("collinsYoudao") : t("simpleYoudao")}</button>)}</div>}
    <div className="sense-grid">{senses.map((sense, index) => <SenseCard key={`${sense.partOfSpeech}-${index}`} label={sense.partOfSpeech === "Collins" ? t("collinsMeaning") : sense.partOfSpeech} definitions={sense.definitions} contentKey={sectionId ? `${sectionId}:sense:${index}` : undefined} expandedContent={expandedContent} toggleExpanded={toggleExpanded} t={t} />)}</div>
    {activeSection && <PhraseSection phrases={activeSection.phrases} contentKey={`${sectionId}:phrases`} expandedContent={expandedContent} toggleExpanded={toggleExpanded} t={t} />}
    <ExampleSection examples={examples} contentKey={sectionId ? `${sectionId}:examples` : undefined} expandedContent={expandedContent} toggleExpanded={toggleExpanded} t={t} />
    {result.note && <div className="source-note"><Icon name="info" size={16} /><span>{note}</span></div>}
  </section>;
}

function LlmCard({ result, query, streaming, addVocabulary, added, t }: { result: LlmLookup; query: string; streaming: boolean; addVocabulary: (entry: VocabularyEntryInput) => void; added: boolean; t: Translator }) {
  const paragraphs = result.content.split(/\n{1,}/).map((paragraph) => paragraph.trim()).filter(Boolean);
  return <section className="entry-card llm-entry"><div className="entry-card-head"><div><div className="word-line"><h2>{displayHeadword(result.word, query)}</h2></div><p className="source-credit">{result.modelName}</p></div><div className="entry-card-tools"><span className="local-mark">{t("localAiGenerated")}</span><VocabularyAddButton added={added} add={() => addVocabulary({ id: null, word: result.word, phonetic: "", definitionMarkdown: `## ${t("definition")}\n\n${result.content}`, examplesMarkdown: "" })} t={t} /></div></div>
    <div className="llm-content">{paragraphs.map((paragraph, index) => <p key={`${index}-${paragraph.slice(0, 24)}`}>{paragraph}</p>)}{streaming && <i className="streaming-caret" aria-hidden="true" />}</div><LlmPerformanceLine performance={result.performance} t={t} /><div className="source-note"><Icon name="info" size={16} /><span>{result.note || t("localAiNote")}</span></div></section>;
}

function EmptyState({ source, query, t }: { source: SourceId; query: string; t: Translator }) {
  const local = source === "local";
  return <div className="empty-state"><div className="empty-orbit"><span /><Icon name={local ? "book" : "globe"} size={28} /></div><h2>{query ? t("noMatch") : t("startWord")}</h2><p>{local ? t("localEmpty") : t("onlineEmpty")}</p></div>;
}

function ResultStage({ state, retry, toggleExpanded, setYoudaoSection, addVocabulary, vocabularyWords, t }: { state: AppState; retry: () => void; toggleExpanded: (key: string) => void; setYoudaoSection: (section: string) => void; addVocabulary: (entry: VocabularyEntryInput) => void; vocabularyWords: Set<string>; t: Translator }) {
  const result = state.sourceResults[state.source];
  const loading = state.pendingSources.has(state.source);
  const error = state.sourceErrors[state.source];
  if (loading && !result) return <div className="loading-state"><div className="aurora-loader"><i /><i /><i /></div><p>{t("loading")}</p></div>;
  if (error && !result) return <div className="error-state"><div className="error-icon"><Icon name="info" size={22} /></div><div><h2>{t("queryFailed")}</h2><p>{error}</p></div><button className="secondary-button" onClick={retry}>{t("retry")}</button></div>;
  if (result?.type === "local") {
    if (!result.result.entries.length) return <EmptyState source={state.source} query={state.query} t={t} />;
    return <>{result.result.sampleData && <div className="sample-banner"><Icon name="info" size={16} /><span>{t("databasePreparing")}</span></div>}<div className="entries">{result.result.entries.map((entry, index) => <EntryCard entry={entry} index={index} query={state.query} expandedContent={state.expandedContent} toggleExpanded={toggleExpanded} addVocabulary={addVocabulary} added={vocabularyWords.has(entry.word.trim().toLocaleLowerCase())} t={t} key={`${entry.word}-${index}`} />)}</div></>;
  }
  if (result?.type === "llm") return <div className="entries"><LlmCard result={result.result} query={state.query} streaming={loading} addVocabulary={addVocabulary} added={vocabularyWords.has(result.result.word.trim().toLocaleLowerCase())} t={t} /></div>;
  if (result?.type === "online") return <div className="entries"><OnlineCard result={result.result} query={state.query} source={state.source} youdaoSection={state.youdaoSection} setYoudaoSection={setYoudaoSection} expandedContent={state.expandedContent} toggleExpanded={toggleExpanded} addVocabulary={addVocabulary} added={vocabularyWords.has(result.result.word.trim().toLocaleLowerCase())} t={t} /></div>;
  return <EmptyState source={state.source} query={state.query} t={t} />;
}

function DictionaryPanel({ state, activeSources, inputValue, setInputValue, suggestions, submit, graphLookup, selectSource, ensureSource, retry, toggleExpanded, setYoudaoSection, addVocabulary, vocabularyWords, t }: {
  state: AppState; activeSources: typeof sources; inputValue: string; setInputValue: (value: string) => void; suggestions: LocalSuggestions | null; submit: (value: string) => void; graphLookup: (value: string) => void; selectSource: (source: SourceId) => void; ensureSource: (source: SourceId) => void; retry: () => void; toggleExpanded: (key: string) => void; setYoudaoSection: (section: string) => void; addVocabulary: (entry: VocabularyEntryInput) => void; vocabularyWords: Set<string>; t: Translator;
}) {
  const handleSubmit = (event: FormEvent) => { event.preventDefault(); submit(inputValue); };
  return <>
    <section className="lookup-zone" aria-label={t("lookupAria")}><form className="search-box" onSubmit={handleSubmit}><span className="search-icon"><Icon name="search" size={22} /></span><input value={inputValue} onChange={(event) => setInputValue(event.target.value)} autoComplete="off" autoCorrect="off" autoCapitalize="none" spellCheck={false} autoFocus placeholder={t("searchPlaceholder")} aria-label={t("searchInputAria")} /><button className="search-submit" type="submit">{t("search")}</button>
      {suggestions && suggestions.suggestions.length > 0 && <div className="input-suggestions" role="listbox" aria-label={t("inputSuggestions")}>{suggestions.correction && <span className="input-suggestions-label">{t("spellingCorrection")}</span>}<div className="input-suggestions-list">{suggestions.suggestions.map((word) => <button type="button" key={word} onClick={() => submit(word)}><span>{word}</span></button>)}</div></div>}
    </form></section>
    <section className="source-section" aria-label={t("selectSource")}><div className="source-switcher" style={{ "--source-count": activeSources.length } as CSSProperties}>{activeSources.map((source) => <button className={`source-tab ${state.source === source.id ? "is-active" : ""}`} key={source.id} type="button" onClick={() => { selectSource(source.id); ensureSource(source.id); }}><span className="source-tab-title">{t(source.title)}</span><span className="source-tab-caption">{t(source.subtitle)}</span></button>)}</div>
    </section>
    <section className="results-stage" aria-live="polite"><ResultStage state={state} retry={retry} toggleExpanded={toggleExpanded} setYoudaoSection={setYoudaoSection} addVocabulary={addVocabulary} vocabularyWords={vocabularyWords} t={t} /></section>
    {state.query && <WordGraphPanel key={state.query} query={state.query} modelId={state.settings.localModel} modelReady={Boolean(state.llmStatus?.engineAvailable && state.llmStatus.models.some((model) => model.modelId === state.settings.localModel && model.installed))} onLookup={graphLookup} t={t} />}
  </>;
}

function TranslationPanel({ state, setInput, submit, t }: { state: AppState; setInput: (value: string) => void; submit: () => void; t: Translator }) {
  const model = localModels.find((item) => item.id === state.settings.localModel)?.name ?? "Qwen3-0.6B";
  return <section className="translation-page" aria-labelledby="translation-title"><header className="panel-page-heading"><div className="modal-icon translation-icon"><Icon name="translate" size={23} /></div><div><p className="eyebrow">LOCAL LLM</p><h2 id="translation-title">{t("translationTitle")}</h2><p className="modal-copy">{t("translationCopy")}</p></div></header>
    <form className="translation-form" onSubmit={(event) => { event.preventDefault(); submit(); }}><textarea maxLength={4000} value={state.translationInput} onChange={(event) => setInput(event.target.value)} placeholder={t("translationPlaceholder")} aria-label={t("translationPlaceholder")} /><div className="translation-form-actions"><span>{model}</span><button className="primary-button" type="submit" disabled={state.translationPending}>{state.translationPending ? t("loading") : t("translate")}</button></div></form>
    <section className="translation-result" aria-live="polite"><header><span>{t("translationResult")}</span><i /></header>{state.translationPending && !state.translationResult && <div className="translation-pending"><div className="aurora-loader"><i /><i /><i /></div><p>{t("loading")}</p></div>}{state.translationError && <p className="translation-error">{state.translationError}</p>}{state.translationResult && <><p className={`translation-output ${state.translationPending ? "is-streaming" : ""}`}>{state.translationResult.translation}{state.translationPending && <i className="streaming-caret" aria-hidden="true" />}</p><LlmPerformanceLine performance={state.translationResult.performance} t={t} />{!state.translationPending && <small>{state.translationResult.note}</small>}</>}{!state.translationPending && !state.translationError && !state.translationResult && <p className="translation-empty">{t("translationEmpty")}</p>}</section>
  </section>;
}

type SettingsUpdater = (updater: (settings: DisplaySettings) => DisplaySettings) => void;

function SourceOrderList({ settings, updateSettings, t }: { settings: DisplaySettings; updateSettings: SettingsUpdater; t: Translator }) {
  const draggedSource = useRef<SourceId | null>(null);
  const dragTarget = useRef<SourceId | null>(null);
  const [dragOver, setDragOver] = useState<SourceId | null>(null);
  const reorder = (source: SourceId, target: SourceId) => {
    if (source === target) return;
    updateSettings((current) => {
      const order = current.sourceOrder.filter((item) => item !== source);
      const targetIndex = order.indexOf(target);
      order.splice(Math.max(targetIndex, 0), 0, source);
      return { ...current, sourceOrder: order };
    });
  };
  const moveByOffset = (source: SourceId, offset: -1 | 1) => updateSettings((current) => {
    const index = current.sourceOrder.indexOf(source);
    const next = index + offset;
    if (index < 0 || next < 0 || next >= current.sourceOrder.length) return current;
    const order = [...current.sourceOrder];
    order.splice(index, 1);
    order.splice(next, 0, source);
    return { ...current, sourceOrder: order };
  });
  const startTouchDrag = (event: ReactPointerEvent<HTMLElement>, source: SourceId) => {
    if (event.pointerType === "mouse" || event.button !== 0) return;
    event.preventDefault();
    draggedSource.current = source;
    const pointerId = event.pointerId;
    event.currentTarget.setPointerCapture(pointerId);
    const move = (moveEvent: PointerEvent) => {
      const target = document.elementFromPoint(moveEvent.clientX, moveEvent.clientY)?.closest<HTMLElement>("[data-source-order]")?.dataset.sourceOrder as SourceId | undefined;
      if (target && target !== source) { dragTarget.current = target; setDragOver(target); }
    };
    const finish = () => {
      if (draggedSource.current && dragTarget.current) reorder(draggedSource.current, dragTarget.current);
      draggedSource.current = null;
      dragTarget.current = null;
      setDragOver(null);
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", finish);
      document.removeEventListener("pointercancel", finish);
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", finish);
    document.addEventListener("pointercancel", finish);
  };
  const startMouseDrag = (event: ReactMouseEvent<HTMLElement>, source: SourceId) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    draggedSource.current = source;
    const move = (moveEvent: MouseEvent) => {
      moveEvent.preventDefault();
      const target = document.elementFromPoint(moveEvent.clientX, moveEvent.clientY)?.closest<HTMLElement>("[data-source-order]")?.dataset.sourceOrder as SourceId | undefined;
      if (target && target !== source) { dragTarget.current = target; setDragOver(target); }
    };
    const finish = (upEvent: MouseEvent) => {
      upEvent.preventDefault();
      if (draggedSource.current && dragTarget.current) reorder(draggedSource.current, dragTarget.current);
      draggedSource.current = null;
      dragTarget.current = null;
      setDragOver(null);
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", finish);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", finish);
  };
  const sourceLimitReached = settings.enabledSources.length >= MAX_ENABLED_SOURCES;
  return <div className="source-settings-grid">{settings.sourceOrder.map((sourceId) => {
    const source = sources.find((item) => item.id === sourceId);
    if (!source) return null;
    const enabled = settings.enabledSources.includes(sourceId);
    const disabled = (!enabled && sourceLimitReached) || (enabled && settings.enabledSources.length === 1);
    return <label className={`source-setting-option ${dragOver === sourceId ? "is-drag-over" : ""} ${disabled ? "is-disabled" : ""}`} data-source-order={sourceId} key={sourceId}
      onDragOver={(event) => { if (draggedSource.current && draggedSource.current !== sourceId) { event.preventDefault(); dragTarget.current = sourceId; setDragOver(sourceId); } }}
      onDrop={(event) => { event.preventDefault(); if (draggedSource.current) reorder(draggedSource.current, sourceId); draggedSource.current = null; dragTarget.current = null; setDragOver(null); }}>
      <input type="checkbox" checked={enabled} disabled={disabled} onChange={(event) => updateSettings((current) => {
        if (!event.target.checked) {
          if (current.enabledSources.length === 1) return current;
          return { ...current, enabledSources: current.enabledSources.filter((item) => item !== sourceId) };
        }
        if (current.enabledSources.includes(sourceId) || current.enabledSources.length >= MAX_ENABLED_SOURCES) return current;
        return { ...current, enabledSources: [...current.enabledSources, sourceId] };
      })} />
      <span className="source-setting-check"><Icon name="check" size={13} /></span><span><b>{t(source.title)}</b><small>{t(source.subtitle)}</small></span>
      <span className="source-order-grip" role="button" draggable tabIndex={0} aria-label={t("defaultFour")}
        onDragStart={(event) => { draggedSource.current = sourceId; event.dataTransfer.effectAllowed = "move"; event.dataTransfer.setData("text/plain", sourceId); }}
        onDragEnd={() => { draggedSource.current = null; dragTarget.current = null; setDragOver(null); }} onMouseDown={(event) => startMouseDrag(event, sourceId)} onPointerDown={(event) => startTouchDrag(event, sourceId)}
        onClick={(event) => { event.preventDefault(); event.stopPropagation(); }}
        onKeyDown={(event) => { if (["ArrowUp", "ArrowLeft", "ArrowDown", "ArrowRight"].includes(event.key)) { event.preventDefault(); event.stopPropagation(); moveByOffset(sourceId, event.key === "ArrowUp" || event.key === "ArrowLeft" ? -1 : 1); } }}><i className="fa-solid fa-grip-vertical" aria-hidden="true" /></span>
    </label>;
  })}</div>;
}

function SettingsPanel({ state, fonts, updateSettings, setTab, setPanel, downloadModel, resetSettings, saveSettings, settingsSaved, t }: {
  state: AppState; fonts: Array<{ id: FontId; label: string }>; updateSettings: SettingsUpdater; setTab: (tab: SettingsTabId) => void; setPanel: (panel: PanelId) => void; downloadModel: (model: LocalModelId) => void; resetSettings: () => void; saveSettings: () => void; settingsSaved: boolean; t: Translator;
}) {
  const settings = state.settings;
  const fontOptions = fonts.some((font) => font.id === settings.font) ? fonts : [...fonts, { id: settings.font, label: settings.font }];
  const resolvedFonts = fontOptions.map((font) => font.id === SYSTEM_FONT_ID ? { ...font, label: t("systemDefault") } : font);
  const installation = (modelId: LocalModelId): LlmModelStatus | undefined => state.llmStatus?.models.find((model) => model.modelId === modelId);
  const progressLabel = (progress: DownloadProgress) => !progress.totalBytes ? (progress.downloadedBytes ? `${t("downloadingModel")} · ${formatBytes(progress.downloadedBytes)}` : t("connectingDownload")) : `${formatBytes(progress.downloadedBytes)} / ${formatBytes(progress.totalBytes)} · ${Math.round(progress.downloadedBytes / progress.totalBytes * 100)}%`;
  return <section className="settings-page" aria-labelledby="settings-title"><h2 id="settings-title" className="sr-only">{t("settingsTitle")}</h2>
    <nav className="settings-tab-switcher" role="tablist" aria-label={t("settingsTitle")}>
      <button className={`settings-category-tab ${state.settingsTab === "appearance" ? "is-active" : ""}`} onClick={() => setTab("appearance")} type="button" role="tab" aria-selected={state.settingsTab === "appearance"}><i className="fa-solid fa-palette" aria-hidden="true" /><span>{t("appearance")}</span></button>
      <button className={`settings-category-tab ${state.settingsTab === "dictionary" ? "is-active" : ""}`} onClick={() => setTab("dictionary")} type="button" role="tab" aria-selected={state.settingsTab === "dictionary"}><i className="fa-solid fa-book" aria-hidden="true" /><span>{t("dictionarySettings")}</span></button>
      <button className={`settings-category-tab ${state.settingsTab === "software" ? "is-active" : ""}`} onClick={() => setTab("software")} type="button" role="tab" aria-selected={state.settingsTab === "software"}><i className="fa-solid fa-circle-info" aria-hidden="true" /><span>{t("softwareInformation")}</span></button>
    </nav>
    {state.settingsTab === "appearance" && <section className="settings-module settings-card settings-card-first" role="tabpanel" aria-labelledby="appearance-section-title"><h3 id="appearance-section-title" className="settings-module-heading">{t("appearance")}</h3>
      <section className="settings-section font-section language-section"><label className="settings-label" htmlFor="ui-language"><span>{t("uiLanguage")}</span><small>{settings.language === "zh" ? t("chinese") : t("english")}</small></label><div className="native-select-wrap"><select id="ui-language" className="settings-select" value={settings.language} onChange={(event) => updateSettings((current) => ({ ...current, language: event.target.value === "en" ? "en" : "zh" }))} aria-label={t("uiLanguage")}><option value="zh">{t("chinese")}</option><option value="en">{t("english")}</option></select><i className="native-select-icon fa-solid fa-chevron-down" aria-hidden="true" /></div></section>
      <section className="settings-section theme-section"><div className="settings-label"><span>{t("interfaceTheme")}</span><small>{t(themes.find((item) => item.id === settings.theme)?.label ?? "themePurple")}</small></div><div className="theme-grid" role="radiogroup" aria-label={t("interfaceTheme")}>{themes.map((theme) => <button className={`theme-swatch ${settings.theme === theme.id ? "is-selected" : ""}`} key={theme.id} onClick={() => updateSettings((current) => ({ ...current, theme: theme.id }))} role="radio" aria-checked={settings.theme === theme.id} title={t(theme.label)}><i style={{ "--swatch": theme.color } as CSSProperties} /><span>{t(theme.label)}</span></button>)}</div></section>
      <section className="settings-section font-section"><label className="settings-label" htmlFor="ui-font"><span>{t("interfaceFont")}</span><small>{resolvedFonts.find((item) => item.id === settings.font)?.label ?? t("systemDefault")}</small></label><div className="native-select-wrap"><select id="ui-font" className="settings-select" value={settings.font} onChange={(event) => updateSettings((current) => ({ ...current, font: event.target.value }))} aria-label={t("interfaceFont")}>{resolvedFonts.map((font) => <option value={font.id} key={font.id}>{font.label}</option>)}</select><i className="native-select-icon fa-solid fa-chevron-down" aria-hidden="true" /></div></section>
      <section className="settings-section range-section"><label className="settings-label" htmlFor="ui-scale"><span>{t("interfaceScale")}</span><output>{settings.scale.toFixed(2)}×</output></label><input id="ui-scale" className="settings-range" type="range" min="0.5" max="2" step="0.05" value={settings.scale} onChange={(event) => updateSettings((current) => ({ ...current, scale: Number(event.target.value) }))} /></section>
    </section>}
    {state.settingsTab === "dictionary" && <section className="settings-module settings-card" role="tabpanel" aria-labelledby="dictionary-section-title"><h3 id="dictionary-section-title" className="settings-module-heading">{t("dictionarySettings")}</h3>
      <section className="settings-section source-settings-section"><div className="settings-label"><span>{t("displayedDictionaries")}</span><small>{t("defaultFour")}</small></div><SourceOrderList settings={settings} updateSettings={updateSettings} t={t} /></section>
      <section className="settings-section api-key-section"><label className="api-key-setting" htmlFor="merriam-webster-api-key"><span className="settings-label"><span>{t("merriamWebsterApiKey")}</span><small>{t("merriamWebsterApiKeyHint")}</small></span><input id="merriam-webster-api-key" type="password" autoComplete="off" maxLength={128} spellCheck={false} value={settings.merriamWebsterApiKey} placeholder={t("merriamWebsterApiKeyPlaceholder")} onChange={(event) => updateSettings((current) => ({ ...current, merriamWebsterApiKey: event.target.value }))} /></label></section>
      <section className="settings-section local-model-section"><div className="settings-label"><span>{t("localAiModels")}</span><small>{t("localAiModelsHint")}</small></div>
        <label className="model-download-source" htmlFor="llm-download-source"><span className="model-download-source-label">{t("modelDownloadSource")} <small className="model-download-source-hint">{t("mirrorDownloadHint")}</small></span><div className="native-select-wrap"><select id="llm-download-source" className="settings-select" value={settings.llmDownloadSource} onChange={(event) => updateSettings((current) => ({ ...current, llmDownloadSource: event.target.value === "official" ? "official" : "mirror" }))} aria-label={t("modelDownloadSource")}><option value="mirror">{t("mirrorDownloadSource")}</option><option value="official">{t("officialDownloadSource")}</option></select><i className="native-select-icon fa-solid fa-chevron-down" aria-hidden="true" /></div></label>
        <div className="local-model-grid" role="radiogroup" aria-label={t("localAiModels")}>{localModels.map((model) => {
          const installed = installation(model.id)?.installed;
          const selected = Boolean(installed && settings.localModel === model.id);
          const progress = state.llmDownload?.modelId === model.id ? state.llmDownload : null;
          const percent = progress?.totalBytes ? clamp(progress.downloadedBytes / progress.totalBytes * 100, 0, 100) : null;
          const error = state.llmActionError?.modelId === model.id ? state.llmActionError.message : "";
          return <article className={`local-model-option ${selected ? "is-selected" : ""}`} key={model.id}><button className="local-model-select" type="button" role="radio" aria-checked={selected} disabled={!installed} onClick={() => updateSettings((current) => ({ ...current, localModel: model.id }))}><span className="local-model-choice"><span><b>{model.name}</b><small>{t(model.description)}</small></span>{model.recommended && <i>{t("modelRecommended")}</i>}</span><span className="local-model-footprint">{t(model.footprint)}{installed ? ` · ${t("modelDownloaded")}` : ""}</span></button>
            {installed ? (selected ? <button className="local-model-action is-selected-model" type="button" disabled>{t("modelInUse")}</button> : <button className="quiet-button local-model-action" type="button" onClick={() => updateSettings((current) => ({ ...current, localModel: model.id }))}>{t("useModel")}</button>) : <button className="primary-button local-model-action" type="button" disabled={state.llmActionPending} onClick={() => downloadModel(model.id)}>{state.llmActionPending && progress ? (percent === null ? t("connectingDownload") : `${t("downloadingModel")} · ${Math.round(percent)}%`) : t("downloadModel")}</button>}
            {progress && <div className="local-model-progress" aria-live="polite"><div className="local-model-progress-heading"><span>{t("downloadingModel")}</span><strong>{percent === null ? "…" : `${Math.round(percent)}%`}</strong></div><div className={`local-model-progress-track ${percent === null ? "is-indeterminate" : ""}`} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent === null ? undefined : Math.round(percent)}><i style={{ "--download-progress": `${percent ?? 35}%` } as CSSProperties} /></div><small>{progressLabel(progress)}</small></div>}
            {error && <p className="local-model-error" role="alert">{error}</p>}
          </article>;
        })}</div><p className="local-model-note">{t("localModelReadyLater")}</p>{state.llmStatus && !state.llmStatus.engineAvailable && <p className="local-model-engine-note">{state.llmStatus.message || t("localAiUnavailable")}</p>}
      </section>
      <section className="settings-section prompt-settings-section"><div className="settings-label"><span>{t("localAiPromptSettings")}</span><small>System Prompt</small></div><p className="prompt-settings-hint">{t("localAiPromptHint")}</p><div className="prompt-editor-grid">
        <label className="prompt-editor"><span><b>{t("dictionarySystemPrompt")}</b><button type="button" onClick={() => updateSettings((current) => ({ ...current, dictionarySystemPrompt: DEFAULT_DICTIONARY_SYSTEM_PROMPT }))}>{t("restoreDefaultPrompt")}</button></span><textarea maxLength={8000} spellCheck={false} value={settings.dictionarySystemPrompt} onChange={(event) => updateSettings((current) => ({ ...current, dictionarySystemPrompt: event.target.value }))} /></label>
        <label className="prompt-editor"><span><b>{t("translationSystemPrompt")}</b><button type="button" onClick={() => updateSettings((current) => ({ ...current, translationSystemPrompt: DEFAULT_TRANSLATION_SYSTEM_PROMPT }))}>{t("restoreDefaultPrompt")}</button></span><textarea maxLength={8000} spellCheck={false} value={settings.translationSystemPrompt} onChange={(event) => updateSettings((current) => ({ ...current, translationSystemPrompt: event.target.value }))} /></label>
      </div></section>
      <section className="settings-section font-section"><label className="settings-label" htmlFor="ui-cache-limit"><span>{t("cacheStorage")}</span><small>{formatText(t, "cacheWords", { count: String(settings.cacheLimit) })}</small></label><div className="native-select-wrap"><select id="ui-cache-limit" className="settings-select" value={settings.cacheLimit} onChange={(event) => updateSettings((current) => ({ ...current, cacheLimit: normaliseCacheLimit(Number(event.target.value)) }))} aria-label={t("cacheStorage")}>{CACHE_LIMIT_OPTIONS.map((limit) => <option value={limit} key={limit}>{formatText(t, "cacheWords", { count: String(limit) })}</option>)}</select><i className="native-select-icon fa-solid fa-chevron-down" aria-hidden="true" /></div></section>
    </section>}
    {state.settingsTab === "software" && <section className="settings-module settings-card software-information" role="tabpanel" aria-labelledby="software-section-title"><h3 id="software-section-title" className="settings-module-heading">{t("softwareInformation")}</h3><dl className="software-information-list"><div><dt>{t("author")}</dt><dd>TheOneGIS</dd></div><div><dt>{t("contact")}</dt><dd><a href="mailto:614106917@qq.com">614106917@qq.com</a></dd></div><div><dt>{t("homepage")}</dt><dd><a href={homepageUrl} onClick={(event) => { event.preventDefault(); openExternalUrl(homepageUrl); }}>{homepageUrl}</a></dd></div><div className="license-row"><dt>{t("license")}</dt><dd>{t("licenseText")}</dd></div></dl>
      <section className="donation-section" aria-labelledby="support-author-title"><div className="donation-copy"><h4 id="support-author-title">{t("supportAuthor")}</h4><p>{t("supportMessage")}</p></div><div className="donation-codes"><figure><img src={wechatDonationUrl} alt={t("wechatPay")} loading="lazy" /><figcaption><i className="fa-brands fa-weixin" aria-hidden="true" />{t("wechatPay")}</figcaption></figure><figure><img src={alipayDonationUrl} alt={t("alipayPay")} loading="lazy" /><figcaption><i className="fa-solid fa-qrcode" aria-hidden="true" />{t("alipayPay")}</figcaption></figure></div></section>
    </section>}
    <div className="modal-actions settings-actions"><button className="quiet-button" onClick={resetSettings}>{t("reset")}</button>{state.settingsTab === "software"
      ? <button className="primary-button" onClick={() => setPanel("dictionary")}>{t("backToDictionary")}</button>
      : <button className="primary-button" onClick={saveSettings} disabled={settingsSaved}>{t("saveSettings")}</button>}</div>
  </section>;
}

async function fetchSourceLookup(source: SourceId, query: string, settings: DisplaySettings, t: Translator, requestId?: string): Promise<SourceLookupResult> {
  if (source === "local") {
    if (isTauri()) return { type: "local", result: await invoke("lookup_local", { query }) };
    return { type: "local", result: query.toLowerCase() === "serendipity" ? fallbackLookup : { ...fallbackLookup, query, entries: [], suggestions: [] } };
  }
  if (source === "local_llm") {
    const fingerprint = promptFingerprint(settings.dictionarySystemPrompt, DEFAULT_DICTIONARY_SYSTEM_PROMPT);
    if (isTauri()) {
      const result = await invoke<LlmLookup>("lookup_llm", { query, modelId: settings.localModel, systemPrompt: settings.dictionarySystemPrompt, requestId });
      return { type: "llm", result: { ...result, promptFingerprint: fingerprint } };
    }
    return { type: "llm", result: { word: query, modelId: settings.localModel, modelName: localModels.find((model) => model.id === settings.localModel)?.name ?? "Qwen3-0.6B", content: "释义：本地 AI 词典结果会在桌面应用中生成。\n用法：浏览器预览不会加载本地模型。", note: t("localAiNote"), promptFingerprint: fingerprint } };
  }
  return { type: "online", result: await invoke<OnlineLookup>("lookup_online", { provider: source, query, apiKey: source === "merriam_webster" ? settings.merriamWebsterApiKey : null }) };
}

function activeSourcesFor(settings: DisplaySettings) {
  const active = settings.sourceOrder.map((id) => sources.find((source) => source.id === id)).filter((source): source is (typeof sources)[number] => Boolean(source && settings.enabledSources.includes(source.id)));
  return (active.length ? active : [sources[0]]).slice(0, MAX_ENABLED_SOURCES);
}

export default function App() {
  const initialSettings = useMemo(() => loadSettings(), []);
  const [state, setState] = useState<AppState>(() => ({
    source: activeSourcesFor(initialSettings).some((source) => source.id === "local") ? "local" : activeSourcesFor(initialSettings)[0].id,
    query: "", pendingSources: new Set(), sourceErrors: {}, sourceResults: {}, panel: "dictionary", settingsTab: "appearance", youdaoSection: "simple", expandedContent: new Set(), settings: initialSettings,
    llmStatus: null, llmDownload: null, llmActionPending: false, llmActionError: null, translationInput: "", translationResult: null, translationPending: false, translationError: "",
  }));
  const [fonts, setFonts] = useState(initialFonts);
  const [inputValue, setInputValue] = useState("");
  const [suggestions, setSuggestions] = useState<LocalSuggestions | null>(null);
  const [lookupHistory, setLookupHistory] = useState<readonly LookupLocation[]>([]);
  const [settingsSaved, setSettingsSaved] = useState(true);
  const [vocabularyEntries, setVocabularyEntries] = useState<VocabularyEntry[]>([]);
  const [vocabularyStatus, setVocabularyStatus] = useState<VocabularyBookStatus | null>(null);
  const [vocabularyLoading, setVocabularyLoading] = useState(false);
  const [vocabularyError, setVocabularyError] = useState("");
  const [selectionMenu, setSelectionMenu] = useState<SelectionMenuState | null>(null);
  const stateRef = useRef(state);
  const contentPanelRef = useRef<HTMLDivElement>(null);
  const lookupSerial = useRef(0);
  const suggestionSerial = useRef(0);
  const suggestionCache = useRef(new Map<string, LocalSuggestions>());
  const scrollTimer = useRef<number | undefined>(undefined);
  const downloadRenderFrame = useRef<number | undefined>(undefined);
  const latestDownloadProgress = useRef<DownloadProgress | null>(null);
  const streamRenderFrame = useRef<number | undefined>(undefined);
  const latestStreamUpdates = useRef<Partial<Record<LlmStreamUpdate["operation"], LlmStreamUpdate>>>({});
  const activeLookupStream = useRef<{ requestId: string; query: string; promptFingerprint: string } | null>(null);
  const activeTranslationStream = useRef<{ requestId: string; source: string } | null>(null);
  const preparedModel = useRef<LocalModelId | null>(null);
  const preparingModel = useRef<LocalModelId | null>(null);
  stateRef.current = state;

  const t = useCallback<Translator>((key) => copy[state.settings.language][key], [state.settings.language]);
  const generateVocabularyQuizQuestion = useCallback((entryId: number): Promise<VocabularyQuizQuestion> => {
    if (!isTauri()) return Promise.reject(new Error("本地 AI 练习仅在桌面应用中可用。"));
    return invoke<VocabularyQuizQuestion>("generate_vocabulary_quiz_question", { entryId, modelId: state.settings.localModel });
  }, [state.settings.localModel]);
  const activeSources = useMemo(() => activeSourcesFor(state.settings), [state.settings]);
  const vocabularyWords = useMemo(() => new Set(vocabularyEntries.map((entry) => entry.word.trim().toLocaleLowerCase())), [vocabularyEntries]);

  useEffect(() => {
    const root = document.documentElement;
    const platform = platformName();
    root.dataset.theme = state.settings.theme;
    root.dataset.platform = platform;
    root.lang = state.settings.language === "zh" ? "zh-CN" : "en";
    root.style.setProperty("--ui-scale", state.settings.scale.toFixed(2));
    const defaultFont = systemFontStacks[platform];
    const font = state.settings.font === SYSTEM_FONT_ID ? defaultFont : `"${state.settings.font.replaceAll('"', '\\"')}", ${defaultFont}`;
    root.style.setProperty("--ui-font", font);
    root.style.setProperty("--word-font", font);
  }, [state.settings.theme, state.settings.language, state.settings.scale, state.settings.font]);

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let stopListening: (() => void) | undefined;
    const appWindow = getCurrentWindow();
    const syncWindowFrame = async () => {
      const [maximized, fullscreen] = await Promise.all([
        appWindow.isMaximized(), appWindow.isFullscreen(),
      ]);
      if (!cancelled) document.documentElement.dataset.windowExpanded = String(maximized || fullscreen);
    };
    void appWindow.onResized(() => { void syncWindowFrame().catch(() => undefined); }).then((unlisten) => {
      if (cancelled) unlisten();
      else stopListening = unlisten;
    }).catch(() => undefined);
    const revealWindowAfterFirstPaint = async () => {
      await syncWindowFrame();
      // A hidden WebView may suspend requestAnimationFrame entirely. The effect
      // itself already runs after React commits the first DOM tree, so only give
      // fonts a short opportunity to settle before revealing the native window.
      await Promise.race([
        document.fonts.ready.catch(() => undefined),
        new Promise<void>((resolve) => window.setTimeout(resolve, 250)),
      ]);
      if (cancelled) return;
      await appWindow.show();
      await appWindow.setFocus();
    };
    void revealWindowAfterFirstPaint().catch(() => undefined);
    return () => { cancelled = true; stopListening?.(); };
  }, []);

  useEffect(() => {
    if (!isTauri() || platformName() !== "linux") return;
    const root = document.documentElement;
    // Include the visible edge, not just the transparent shadow gutter.
    // Coordinates are viewport pixels, so hit targets stay usable at any UI scale.
    const resizeDirection = (event: PointerEvent) => {
      if (root.dataset.windowExpanded === "true" || root.dataset.windowTiled === "true") return null;
      const shell = document.querySelector(".app-shell")?.getBoundingClientRect();
      if (!shell) return null;
      const { clientX: x, clientY: y } = event;
      const corner = 16;
      const edge = 6;
      if (x <= shell.left + corner && y <= shell.top + corner) return "NorthWest";
      if (x >= shell.right - corner && y <= shell.top + corner) return "NorthEast";
      if (x <= shell.left + corner && y >= shell.bottom - corner) return "SouthWest";
      if (x >= shell.right - corner && y >= shell.bottom - corner) return "SouthEast";
      if (y <= shell.top + edge) return "North";
      if (y >= shell.bottom - edge) return "South";
      if (x <= shell.left + edge) return "West";
      if (x >= shell.right - edge) return "East";
      return null;
    };
    const clearResizeCursor = () => { delete root.dataset.windowResize; };
    const updateResizeCursor = (event: PointerEvent) => {
      const direction = resizeDirection(event);
      if (direction) root.dataset.windowResize = direction;
      else clearResizeCursor();
    };
    const resizeFromEdge = (event: PointerEvent) => {
      if (event.button !== 0) return;
      const direction = resizeDirection(event);
      if (!direction) return;
      event.preventDefault();
      event.stopPropagation();
      void getCurrentWindow().startResizeDragging(direction).catch(clearResizeCursor);
    };
    document.addEventListener("pointerdown", resizeFromEdge, true);
    document.addEventListener("pointermove", updateResizeCursor);
    document.addEventListener("pointerup", clearResizeCursor, true);
    document.documentElement.addEventListener("pointerleave", clearResizeCursor);
    window.addEventListener("blur", clearResizeCursor);
    return () => {
      document.removeEventListener("pointerdown", resizeFromEdge, true);
      document.removeEventListener("pointermove", updateResizeCursor);
      document.removeEventListener("pointerup", clearResizeCursor, true);
      document.documentElement.removeEventListener("pointerleave", clearResizeCursor);
      window.removeEventListener("blur", clearResizeCursor);
      clearResizeCursor();
    };
  }, []);

  const updateSettings = useCallback<SettingsUpdater>((updater) => {
    setSettingsSaved(false);
    setState((current) => {
      const settings = updater(current.settings);
      if (settings === current.settings) return current;
      const visible = activeSourcesFor(settings);
      const source = visible.some((item) => item.id === current.source) ? current.source : visible[0].id;
      const llmChanged = settings.localModel !== current.settings.localModel || settings.dictionarySystemPrompt !== current.settings.dictionarySystemPrompt;
      const translationChanged = settings.localModel !== current.settings.localModel || settings.translationSystemPrompt !== current.settings.translationSystemPrompt;
      const sourceResults = { ...current.sourceResults };
      const sourceErrors = { ...current.sourceErrors };
      if (llmChanged) { delete sourceResults.local_llm; delete sourceErrors.local_llm; }
      return { ...current, settings, source, sourceResults, sourceErrors, llmActionError: settings.localModel !== current.settings.localModel ? null : current.llmActionError, translationResult: translationChanged ? null : current.translationResult, translationError: translationChanged ? "" : current.translationError };
    });
  }, []);

  const loadVocabulary = useCallback(async () => {
    setVocabularyLoading(true);
    setVocabularyError("");
    try {
      if (!isTauri()) {
        setVocabularyStatus({ path: "Browser preview / aurora-vocabulary.sqlite3", entryCount: vocabularyEntries.length });
        return;
      }
      const [entries, status] = await Promise.all([
        invoke<VocabularyEntry[]>("list_vocabulary_entries", { search: null }),
        invoke<VocabularyBookStatus>("vocabulary_book_status"),
      ]);
      setVocabularyEntries(entries);
      setVocabularyStatus(status);
    } catch (error) {
      setVocabularyError(error instanceof Error ? error.message : String(error));
    } finally { setVocabularyLoading(false); }
  }, [vocabularyEntries.length]);

  const persistVocabularyEntry = useCallback(async (entry: VocabularyEntryInput): Promise<VocabularyEntry> => {
    const saved = isTauri()
      ? await invoke<VocabularyEntry>(entry.id === null ? "add_vocabulary_entry" : "save_vocabulary_entry", { entry })
      : { ...entry, id: entry.id ?? Date.now(), createdAt: Date.now(), updatedAt: Date.now() } as VocabularyEntry;
    setVocabularyEntries((current) => {
      const withoutSaved = current.filter((item) => item.id !== saved.id);
      return [saved, ...withoutSaved].sort((left, right) => right.updatedAt - left.updatedAt);
    });
    setVocabularyStatus((current) => current ? { ...current, entryCount: vocabularyEntries.some((item) => item.id === saved.id) ? current.entryCount : current.entryCount + 1 } : current);
    setVocabularyError("");
    return saved;
  }, [vocabularyEntries]);

  const addVocabularyFromLookup = useCallback((entry: VocabularyEntryInput) => {
    void persistVocabularyEntry(entry).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      setVocabularyError(message);
      window.alert(message);
    });
  }, [persistVocabularyEntry]);

  const deleteVocabularyEntry = useCallback(async (id: number) => {
    if (isTauri()) await invoke("delete_vocabulary_entry", { id });
    setVocabularyEntries((current) => current.filter((entry) => entry.id !== id));
    setVocabularyStatus((current) => current ? { ...current, entryCount: Math.max(0, current.entryCount - 1) } : current);
  }, []);

  const runVocabularyFileCommand = useCallback(async (command: string, argument: Record<string, string>): Promise<VocabularyBookStatus> => {
    if (!isTauri()) throw new Error("File operations are only available in the desktop app.");
    const status = await invoke<VocabularyBookStatus>(command, argument);
    setVocabularyStatus(status);
    setVocabularyError("");
    return status;
  }, []);

  useEffect(() => { void loadVocabulary(); }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const loadLlmStatus = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const status = await invoke<LlmStatus>("llm_status");
      const selectedInstalled = status.models.some((model) => model.modelId === stateRef.current.settings.localModel && model.installed);
      const fallbackModel = selectedInstalled ? null : status.models.find((model) => model.installed)?.modelId;
      if (fallbackModel) setSettingsSaved(false);
      setState((current) => {
        if (!fallbackModel || status.models.some((model) => model.modelId === current.settings.localModel && model.installed)) return { ...current, llmStatus: status };
        const settings = { ...current.settings, localModel: fallbackModel };
        const sourceResults = { ...current.sourceResults };
        const sourceErrors = { ...current.sourceErrors };
        delete sourceResults.local_llm;
        delete sourceErrors.local_llm;
        return { ...current, settings, llmStatus: status, sourceResults, sourceErrors, llmActionError: null, translationResult: null, translationError: "" };
      });
    } catch {
      setState((current) => ({ ...current, llmStatus: null }));
    }
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    void invoke<DictionaryStatus>("dictionary_status").catch(() => undefined);
    void loadLlmStatus();
    let cancelled = false;
    let stopDownloadListening: (() => void) | undefined;
    let stopStreamListening: (() => void) | undefined;
    void listen<DownloadProgress>("llm-download-progress", (event) => {
      latestDownloadProgress.current = event.payload;
      if (downloadRenderFrame.current !== undefined) return;
      downloadRenderFrame.current = window.requestAnimationFrame(() => {
        downloadRenderFrame.current = undefined;
        setState((current) => ({ ...current, llmDownload: latestDownloadProgress.current }));
      });
    }).then((unlisten) => { if (cancelled) unlisten(); else stopDownloadListening = unlisten; });
    void listen<LlmStreamUpdate>("llm-stream-update", (event) => {
      const payload = event.payload;
      if (payload.operation === "lookup" && activeLookupStream.current?.requestId !== payload.requestId) return;
      if (payload.operation === "translation" && activeTranslationStream.current?.requestId !== payload.requestId) return;
      latestStreamUpdates.current[payload.operation] = payload;
      if (streamRenderFrame.current !== undefined) return;
      streamRenderFrame.current = window.requestAnimationFrame(() => {
        streamRenderFrame.current = undefined;
        const updates = latestStreamUpdates.current;
        latestStreamUpdates.current = {};
        setState((current) => {
          let next = current;
          const lookup = updates.lookup;
          const lookupContext = activeLookupStream.current;
          if (lookup && lookupContext?.requestId === lookup.requestId && lookup.content) {
            next = {
              ...next,
              sourceResults: {
                ...next.sourceResults,
                local_llm: {
                  type: "llm",
                  result: {
                    word: lookupContext.query,
                    modelId: lookup.modelId,
                    modelName: lookup.modelName,
                    content: lookup.content,
                    note: copy[next.settings.language].localAiNote,
                    promptFingerprint: lookupContext.promptFingerprint,
                    performance: lookup.performance,
                  },
                },
              },
            };
          }
          const translation = updates.translation;
          const translationContext = activeTranslationStream.current;
          if (translation && translationContext?.requestId === translation.requestId) {
            next = {
              ...next,
              translationResult: translation.reset || !translation.content ? null : {
                source: translationContext.source,
                translation: translation.content,
                modelId: translation.modelId,
                modelName: translation.modelName,
                note: copy[next.settings.language].localAiNote,
                performance: translation.performance,
              },
            };
          }
          return next;
        });
      });
    }).then((unlisten) => { if (cancelled) unlisten(); else stopStreamListening = unlisten; });
    return () => {
      cancelled = true;
      stopDownloadListening?.();
      stopStreamListening?.();
      if (downloadRenderFrame.current !== undefined) window.cancelAnimationFrame(downloadRenderFrame.current);
      if (streamRenderFrame.current !== undefined) window.cancelAnimationFrame(streamRenderFrame.current);
    };
  }, [loadLlmStatus]);

  useEffect(() => {
    if (state.panel !== "settings") return;
    void loadLlmStatus();
    if (state.settingsTab !== "appearance" || !isTauri() || fonts.length > 1) return;
    void getSystemFonts().then((systemFonts) => {
      const names = [...new Set(systemFonts.map((font) => font.name.trim()).filter(Boolean))].sort((left, right) => left.localeCompare(right, "zh-Hans-CN"));
      setFonts([{ id: SYSTEM_FONT_ID, label: "" }, ...names.map((name) => ({ id: name, label: name }))]);
    }).catch(() => undefined);
  }, [state.panel, state.settingsTab, fonts.length, loadLlmStatus]);

  useEffect(() => {
    if (!isTauri() || !state.llmStatus?.engineAvailable) return;
    const modelId = state.settings.localModel;
    const installed = state.llmStatus.models.some((model) => model.modelId === modelId && model.installed);
    const shouldPrepare = state.settings.enabledSources.includes("local_llm") || state.panel === "translation" || state.panel === "quiz";
    if (!installed || !shouldPrepare || preparedModel.current === modelId || preparingModel.current === modelId) return;
    const timer = window.setTimeout(() => {
      preparingModel.current = modelId;
      void invoke<LlmPrepareResult>("prepare_llm", { modelId }).then((result) => {
        preparedModel.current = result.modelId;
      }).catch(() => undefined).finally(() => {
        if (preparingModel.current === modelId) preparingModel.current = null;
      });
    }, 600);
    return () => window.clearTimeout(timer);
  }, [state.llmStatus, state.settings.localModel, state.settings.enabledSources, state.panel]);

  useEffect(() => {
    if (!selectionMenu) return;
    const dismissOnPointerDown = (event: PointerEvent) => {
      if (!(event.target instanceof Element) || !event.target.closest(".selection-lookup-menu")) setSelectionMenu(null);
    };
    const dismissOnKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") setSelectionMenu(null); };
    const dismiss = () => setSelectionMenu(null);
    document.addEventListener("pointerdown", dismissOnPointerDown);
    document.addEventListener("keydown", dismissOnKeyDown);
    window.addEventListener("blur", dismiss);
    window.addEventListener("resize", dismiss);
    return () => {
      document.removeEventListener("pointerdown", dismissOnPointerDown);
      document.removeEventListener("keydown", dismissOnKeyDown);
      window.removeEventListener("blur", dismiss);
      window.removeEventListener("resize", dismiss);
    };
  }, [selectionMenu]);

  useEffect(() => {
    const query = inputValue.trim();
    const serial = ++suggestionSerial.current;
    if (!isEnglishInput(query) || query.length < 2 || query === state.query) { setSuggestions(null); return; }
    const cacheKey = query.toLocaleLowerCase();
    const cached = suggestionCache.current.get(cacheKey);
    if (cached) {
      setSuggestions(cached.suggestions.length ? cached : null);
      return;
    }
    const timer = window.setTimeout(async () => {
      try {
        const result = isTauri() ? await invoke<LocalSuggestions>("suggest_local_words", { query }) : { suggestions: fallbackLookup.entries.map((entry) => entry.word).filter((word) => word.startsWith(query.toLowerCase())), correction: false };
        suggestionCache.current.set(cacheKey, result);
        if (serial === suggestionSerial.current && inputValue.trim() === query) setSuggestions(result.suggestions.length ? result : null);
      } catch { if (serial === suggestionSerial.current) setSuggestions(null); }
    }, 120);
    return () => window.clearTimeout(timer);
  }, [inputValue, state.query]);

  const submitLookup = useCallback(async (rawQuery: string, sourceOverride?: SourceId) => {
    const query = rawQuery.trim();
    const serial = ++lookupSerial.current;
    activeLookupStream.current = null;
    suggestionSerial.current += 1;
    setInputValue(query);
    setSuggestions(null);
    if (!query) {
      setState((current) => ({ ...current, query: "", youdaoSection: "simple", expandedContent: new Set(), sourceErrors: {}, sourceResults: {}, pendingSources: new Set() }));
      return;
    }
    const snapshot = stateRef.current;
    const sourceIds = activeSourcesFor(snapshot.settings).map((source) => source.id);
    const preload = sourceIds.filter((source) => source !== "local_llm" || source === (sourceOverride ?? snapshot.source));
    setState((current) => ({ ...current, query, source: sourceOverride ?? current.source, youdaoSection: "simple", expandedContent: new Set(), sourceErrors: {}, sourceResults: {}, pendingSources: new Set(preload) }));
    const cached = await readCachedQuery(query);
    if (serial !== lookupSerial.current) return;
    const results: Partial<Record<SourceId, SourceLookupResult>> = {};
    if (cached) {
      const fingerprint = promptFingerprint(snapshot.settings.dictionarySystemPrompt, DEFAULT_DICTIONARY_SYSTEM_PROMPT);
      sourceIds.forEach((source) => {
        const result = cached.results[source];
        if (result && (source !== "local_llm" || (result.type === "llm" && result.result.modelId === snapshot.settings.localModel && result.result.promptFingerprint === fingerprint))) results[source] = result;
      });
    }
    const missing = preload.filter((source) => !results[source]);
    setState((current) => ({ ...current, sourceResults: results, pendingSources: new Set(missing) }));
    const successful: Partial<Record<SourceId, SourceLookupResult>> = {};
    await Promise.all(missing.map(async (source) => {
      const requestId = source === "local_llm" ? llmRequestId("lookup") : undefined;
      if (requestId) activeLookupStream.current = { requestId, query, promptFingerprint: promptFingerprint(snapshot.settings.dictionarySystemPrompt, DEFAULT_DICTIONARY_SYSTEM_PROMPT) };
      try {
        const result = await fetchSourceLookup(source, query, snapshot.settings, (key) => copy[snapshot.settings.language][key], requestId);
        if (serial !== lookupSerial.current) return;
        successful[source] = result;
        setState((current) => ({ ...current, sourceResults: { ...current.sourceResults, [source]: result } }));
        if (source === "local" && result.type === "local" && result.result.suggestions.length) setSuggestions({ suggestions: result.result.suggestions, correction: true });
      } catch (error) {
        if (serial === lookupSerial.current) setState((current) => {
          const sourceResults = { ...current.sourceResults };
          if (source === "local_llm") delete sourceResults.local_llm;
          return { ...current, sourceResults, sourceErrors: { ...current.sourceErrors, [source]: error instanceof Error ? error.message : String(error) } };
        });
      } finally {
        if (serial === lookupSerial.current) setState((current) => { const pending = new Set(current.pendingSources); pending.delete(source); return { ...current, pendingSources: pending }; });
      }
    }));
    if (serial === lookupSerial.current && Object.keys(successful).length) void cacheSourceResults(query, successful, snapshot.settings.cacheLimit);
  }, []);

  const submitFreshLookup = useCallback((value: string) => {
    setLookupHistory([]);
    void submitLookup(value);
  }, [submitLookup]);

  const lookupFromGraph = useCallback((word: string) => {
    const current = stateRef.current;
    const nextHistory = pushGraphLookup(lookupHistory, { query: current.query, source: current.source }, word);
    if (nextHistory === lookupHistory) return;
    setLookupHistory(nextHistory);
    contentPanelRef.current?.scrollTo(0, 0);
    void submitLookup(word);
  }, [lookupHistory, submitLookup]);

  const returnToPreviousLookup = useCallback(() => {
    const { previous, history } = takePreviousLookup(lookupHistory);
    if (!previous) return;
    setLookupHistory(history);
    contentPanelRef.current?.scrollTo(0, 0);
    void submitLookup(previous.query, previous.source);
  }, [lookupHistory, submitLookup]);

  const ensureSource = useCallback(async (source: SourceId) => {
    const snapshot = stateRef.current;
    const query = snapshot.query.trim();
    if (!query || snapshot.sourceResults[source] || snapshot.pendingSources.has(source)) return;
    const serial = lookupSerial.current;
    const requestId = source === "local_llm" ? llmRequestId("lookup") : undefined;
    if (requestId) activeLookupStream.current = { requestId, query, promptFingerprint: promptFingerprint(snapshot.settings.dictionarySystemPrompt, DEFAULT_DICTIONARY_SYSTEM_PROMPT) };
    setState((current) => { const pending = new Set(current.pendingSources); pending.add(source); const errors = { ...current.sourceErrors }; delete errors[source]; return { ...current, pendingSources: pending, sourceErrors: errors }; });
    try {
      const result = await fetchSourceLookup(source, query, snapshot.settings, (key) => copy[snapshot.settings.language][key], requestId);
      if (serial !== lookupSerial.current) return;
      setState((current) => ({ ...current, sourceResults: { ...current.sourceResults, [source]: result } }));
      void cacheSourceResults(query, { [source]: result }, snapshot.settings.cacheLimit);
    } catch (error) {
      if (serial === lookupSerial.current) setState((current) => {
        const sourceResults = { ...current.sourceResults };
        if (source === "local_llm") delete sourceResults.local_llm;
        return { ...current, sourceResults, sourceErrors: { ...current.sourceErrors, [source]: error instanceof Error ? error.message : String(error) } };
      });
    } finally {
      if (serial === lookupSerial.current) setState((current) => { const pending = new Set(current.pendingSources); pending.delete(source); return { ...current, pendingSources: pending }; });
    }
  }, []);

  const translate = useCallback(async () => {
    const snapshot = stateRef.current;
    const text = snapshot.translationInput.trim();
    if (!text || snapshot.translationPending) return;
    const requestId = llmRequestId("translation");
    activeTranslationStream.current = { requestId, source: text };
    setState((current) => ({ ...current, translationPending: true, translationError: "", translationResult: null }));
    try {
      const result = isTauri() ? await invoke<LlmTranslation>("translate_llm", { text, modelId: snapshot.settings.localModel, systemPrompt: snapshot.settings.translationSystemPrompt, requestId }) : { source: text, translation: "浏览器预览不会加载本地模型。请在桌面应用的离线 AI 版中使用翻译功能。", modelId: snapshot.settings.localModel, modelName: localModels.find((model) => model.id === snapshot.settings.localModel)?.name ?? "Qwen3-0.6B", note: copy[snapshot.settings.language].localAiNote };
      setState((current) => ({ ...current, translationResult: result }));
    } catch (error) {
      setState((current) => ({ ...current, translationResult: null, translationError: error instanceof Error ? error.message : String(error) }));
    } finally { setState((current) => ({ ...current, translationPending: false })); }
  }, []);

  const downloadModel = useCallback(async (modelId: LocalModelId) => {
    const snapshot = stateRef.current;
    if (!isTauri() || snapshot.llmActionPending) return;
    setState((current) => ({ ...current, llmActionPending: true, llmDownload: { modelId, downloadedBytes: 0, complete: false }, llmActionError: null }));
    try {
      await invoke("download_llm_model", { modelId, downloadSource: snapshot.settings.llmDownloadSource });
      preparedModel.current = null;
      updateSettings((current) => ({ ...current, localModel: modelId }));
      await loadLlmStatus();
    } catch (error) {
      setState((current) => ({ ...current, llmActionError: { modelId, message: error instanceof Error ? error.message : String(error) } }));
    } finally { setState((current) => ({ ...current, llmActionPending: false, llmDownload: null })); }
  }, [loadLlmStatus, updateSettings]);

  const resetSettings = useCallback(() => {
    const settings = cloneDefaultSettings();
    setSettingsSaved(false);
    setState((current) => ({ ...current, settings, source: "local", sourceResults: {}, sourceErrors: {}, translationResult: null, translationError: "" }));
  }, []);

  const saveSettings = useCallback(() => {
    const settings = stateRef.current.settings;
    persistSettings(settings);
    void trimQueryCache(settings.cacheLimit);
    setSettingsSaved(true);
  }, []);

  const openSelectionMenu = useCallback((event: ReactMouseEvent<HTMLElement>) => {
    const context = selectionContextInSurface(event.target, event.currentTarget);
    if (!context.text && !context.input) { setSelectionMenu(null); return; }
    event.preventDefault();
    const target = event.target instanceof Element ? event.target : event.currentTarget;
    const rect = target.getBoundingClientRect();
    const anchorX = event.clientX > 0 ? event.clientX : rect.left + 16;
    const anchorY = event.clientY > 0 ? event.clientY : rect.bottom;
    const itemCount = Number(Boolean(context.text)) + Number(Boolean(context.lookupText)) * 2 + Number(Boolean(context.input));
    setSelectionMenu({
      ...context,
      x: clamp(anchorX, 8, Math.max(8, window.innerWidth - 248)),
      y: clamp(anchorY, 8, Math.max(8, window.innerHeight - itemCount * 38 - 12)),
    });
  }, []);

  const copySelection = useCallback((text: string) => {
    setSelectionMenu(null);
    void copyTextToClipboard(text).catch(() => undefined);
  }, []);

  const pasteSelection = useCallback((menu: SelectionMenuState) => {
    setSelectionMenu(null);
    void pasteTextFromClipboard().then((clipboardText) => {
      const input = menu.input;
      if (!input?.isConnected || !clipboardText) return;
      const start = clamp(menu.selectionStart, 0, input.value.length);
      const end = clamp(menu.selectionEnd, start, input.value.length);
      const available = input.maxLength > 0 ? Math.max(0, input.maxLength - (input.value.length - (end - start))) : clipboardText.length;
      const insertion = clipboardText.slice(0, available);
      input.focus();
      input.setRangeText(insertion, start, end, "end");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }).catch(() => undefined);
  }, []);

  const lookupSelection = useCallback((text: string) => {
    setSelectionMenu(null);
    setState((current) => ({ ...current, panel: "dictionary" }));
    submitFreshLookup(text);
  }, [submitFreshLookup]);

  const addSelectionToVocabulary = useCallback((text: string) => {
    setSelectionMenu(null);
    addVocabularyFromLookup({ id: null, word: text, phonetic: "", definitionMarkdown: `## ${t("definition")}\n\n- `, examplesMarkdown: "" });
  }, [addVocabularyFromLookup, t]);

  const handleScroll = () => {
    setSelectionMenu(null);
    const root = document.querySelector("#app");
    root?.classList.add("is-scrolling");
    if (scrollTimer.current) window.clearTimeout(scrollTimer.current);
    scrollTimer.current = window.setTimeout(() => root?.classList.remove("is-scrolling"), 720);
  };

  return <div className="app-shell"><TitleBar t={t} /><div className="content-panel" ref={contentPanelRef} onScroll={handleScroll}><main onContextMenu={openSelectionMenu}><Hero panel={state.panel} setPanel={(panel) => setState((current) => ({ ...current, panel }))} previousWord={lookupHistory.at(-1)?.query ?? null} returnToPrevious={returnToPreviousLookup} t={t} />
    {state.panel === "dictionary" && <div className="selection-lookup-surface dictionary-page"><DictionaryPanel state={state} activeSources={activeSources} inputValue={inputValue} setInputValue={setInputValue} suggestions={suggestions} submit={submitFreshLookup} graphLookup={lookupFromGraph} selectSource={(source) => setState((current) => ({ ...current, source, youdaoSection: "simple", expandedContent: new Set() }))} ensureSource={(source) => void ensureSource(source)} retry={() => void submitLookup(state.query)} toggleExpanded={(key) => setState((current) => { const expanded = new Set(current.expandedContent); if (expanded.has(key)) expanded.delete(key); else expanded.add(key); return { ...current, expandedContent: expanded }; })} setYoudaoSection={(youdaoSection) => setState((current) => ({ ...current, youdaoSection }))} addVocabulary={addVocabularyFromLookup} vocabularyWords={vocabularyWords} t={t} /></div>}
    {state.panel === "translation" && <div className="selection-lookup-surface"><TranslationPanel state={state} setInput={(translationInput) => setState((current) => ({ ...current, translationInput }))} submit={() => void translate()} t={t} /></div>}
    {state.panel === "vocabulary" && <div className="selection-lookup-surface"><VocabularyPanel entries={vocabularyEntries} status={vocabularyStatus} loading={vocabularyLoading} error={vocabularyError} isDesktop={isTauri()} persist={persistVocabularyEntry} remove={deleteVocabularyEntry} refresh={loadVocabulary} importBook={(path) => runVocabularyFileCommand("import_vocabulary_book", { sourcePath: path })} exportBook={(path) => runVocabularyFileCommand("export_vocabulary_book", { destinationPath: path })} moveBook={(path) => runVocabularyFileCommand("move_vocabulary_book", { destinationPath: path })} openBook={(path) => runVocabularyFileCommand("open_vocabulary_book", { sourcePath: path })} t={t} /></div>}
    <div className="selection-lookup-surface" hidden={state.panel !== "quiz"}><VocabularyQuiz entries={vocabularyEntries} modelReady={Boolean(isTauri() && state.llmStatus?.engineAvailable && state.llmStatus.models.some((model) => model.modelId === state.settings.localModel && model.installed))} modelName={localModels.find((model) => model.id === state.settings.localModel)?.name ?? "Qwen3-0.6B"} generateQuestion={generateVocabularyQuizQuestion} openSettings={() => setState((current) => ({ ...current, panel: "settings", settingsTab: "dictionary" }))} t={t} /></div>
    {state.panel === "settings" && <SettingsPanel state={state} fonts={fonts} updateSettings={updateSettings} setTab={(settingsTab) => setState((current) => ({ ...current, settingsTab }))} setPanel={(panel) => setState((current) => ({ ...current, panel }))} downloadModel={(model) => void downloadModel(model)} resetSettings={resetSettings} saveSettings={saveSettings} settingsSaved={settingsSaved} t={t} />}
  </main></div>{selectionMenu && <SelectionLookupMenu menu={selectionMenu} copySelection={copySelection} lookup={lookupSelection} addVocabulary={addSelectionToVocabulary} paste={pasteSelection} t={t} />}<footer><span className="footer-pulse" /><span>{t("footerLocalFirst")}</span><i /><span>{t("footerLearning")}</span></footer></div>;
}
