import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChangeEvent, KeyboardEvent as ReactKeyboardEvent, MouseEvent as ReactMouseEvent, ReactNode } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { Icon } from "./Icon";
import { playPronunciation } from "./audio";
import { PronunciationRow } from "./Pronunciation";
import type { CopyKey } from "./config";
import type { VocabularyBookStatus, VocabularyEntry, VocabularyEntryInput } from "./types";

type Translator = (key: CopyKey) => string;

const sqliteFilters = [{ name: "Aurora Dict SQLite", extensions: ["sqlite3", "sqlite", "db"] }];

function inlineMarkdown(text: string): ReactNode[] {
  const tokens = text.split(/(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*)/g).filter(Boolean);
  return tokens.map((token, index) => {
    if (token.startsWith("`") && token.endsWith("`")) return <code key={index}>{token.slice(1, -1)}</code>;
    if (token.startsWith("**") && token.endsWith("**")) return <strong key={index}>{token.slice(2, -2)}</strong>;
    if (token.startsWith("*") && token.endsWith("*")) return <em key={index}>{token.slice(1, -1)}</em>;
    return token;
  });
}

function markdownSections(value: string): Array<{ level: number; title: string | null; lines: string[] }> {
  const sections: Array<{ level: number; title: string | null; lines: string[] }> = [];
  let current = { level: 0, title: null as string | null, lines: [] as string[] };
  for (const line of value.split("\n")) {
    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      if (current.title || current.lines.some((item) => item.trim())) sections.push(current);
      current = { level: heading[1].length, title: heading[2], lines: [] };
    } else {
      current.lines.push(line);
    }
  }
  if (current.title || current.lines.some((item) => item.trim())) sections.push(current);
  return sections;
}

function ReadingMarkdown({ value }: { value: string }) {
  const sections = markdownSections(value);
  return <div className="vocabulary-read-modules">{sections.map((section, index) => {
    const body = section.lines.join("\n").trim();
    const next = sections[index + 1];
    if (section.level > 0 && section.level <= 2 && !body && next && next.level > section.level) {
      return <h2 className="vocabulary-read-label" key={index}>{inlineMarkdown(section.title ?? "")}</h2>;
    }
    return <section className="vocabulary-sense-card" key={index}>
      {section.title && (section.level <= 2 ? <h2>{inlineMarkdown(section.title)}</h2> : <h3>{inlineMarkdown(section.title)}</h3>)}
      {body && <MarkdownPreview value={body} emptyText="" />}
    </section>;
  })}</div>;
}

function MarkdownPreview({ value, emptyText }: { value: string; emptyText: string }) {
  const lines = value.split("\n");
  if (!value.trim()) return <p className="markdown-preview-empty">{emptyText}</p>;
  return <div className="markdown-preview-content">{lines.map((line, index) => {
    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      const content = inlineMarkdown(heading[2]);
      if (heading[1].length === 1) return <h1 key={index}>{content}</h1>;
      if (heading[1].length === 2) return <h2 key={index}>{content}</h2>;
      return <h3 key={index}>{content}</h3>;
    }
    const unordered = line.match(/^\s*[-*+]\s+(.+)$/);
    if (unordered) return <div className="markdown-list-item" key={index}><i aria-hidden="true" /> <span>{inlineMarkdown(unordered[1])}</span></div>;
    const ordered = line.match(/^\s*(\d+)\.\s+(.+)$/);
    if (ordered) return <div className="markdown-list-item is-ordered" key={index}><b>{ordered[1]}.</b><span>{inlineMarkdown(ordered[2])}</span></div>;
    const quote = line.match(/^>\s?(.*)$/);
    if (quote) return <blockquote key={index}>{inlineMarkdown(quote[1])}</blockquote>;
    if (!line.trim()) return <div className="markdown-spacer" key={index} />;
    return <p key={index}>{inlineMarkdown(line)}</p>;
  })}</div>;
}

function MarkdownField({ label, value, placeholder, emptyText, template, templateLabel, editLabel, previewLabel, onChange }: {
  label: string;
  value: string;
  placeholder: string;
  emptyText: string;
  template: string;
  templateLabel: string;
  editLabel: string;
  previewLabel: string;
  onChange: (value: string) => void;
}) {
  const insertTemplate = () => onChange(value.trim() ? `${value.trimEnd()}\n\n${template}` : template);
  return <section className="markdown-field">
    <header><div><b>{label}</b><small>Markdown</small></div><button type="button" onClick={insertTemplate}><i className="fa-solid fa-wand-magic-sparkles" aria-hidden="true" />{templateLabel}</button></header>
    <div className="markdown-editor-grid">
      <label className="markdown-source"><span className="markdown-pane-label"><i className="fa-solid fa-pen" aria-hidden="true" />{editLabel}</span><textarea aria-label={label} value={value} maxLength={100000} spellCheck onChange={(event) => onChange(event.target.value)} placeholder={placeholder} /></label>
      <section className="markdown-preview" aria-label={`${label} · ${previewLabel}`}><span className="markdown-pane-label"><i className="fa-regular fa-eye" aria-hidden="true" />{previewLabel}</span><MarkdownPreview value={value} emptyText={emptyText} /></section>
    </div>
  </section>;
}

function sameEntry(left: VocabularyEntryInput, right: VocabularyEntryInput): boolean {
  return left.id === right.id && left.word === right.word && left.phonetic === right.phonetic
    && left.definitionMarkdown === right.definitionMarkdown && left.examplesMarkdown === right.examplesMarkdown;
}

function editableEntry(entry: VocabularyEntry): VocabularyEntryInput {
  return {
    id: entry.id,
    word: entry.word,
    phonetic: entry.phonetic,
    definitionMarkdown: entry.definitionMarkdown,
    examplesMarkdown: entry.examplesMarkdown,
  };
}

const emptyEntry = (): VocabularyEntryInput => ({ id: null, word: "", phonetic: "", definitionMarkdown: "", examplesMarkdown: "" });

function definitionExcerpt(markdown: string): string {
  const line = markdown.split("\n").map((item) => item.trim()).find((item) => item && !item.startsWith("#") && !item.startsWith("```"));
  if (!line) return "";
  return line.replace(/^[-*+]\s+/, "").replace(/^\d+\.\s+/, "").replace(/^>\s?/, "").replace(/[*_`]/g, "").trim();
}

export function VocabularyPanel({ entries, status, loading, error, isDesktop, persist, remove, refresh, importBook, exportBook, moveBook, openBook, t }: {
  entries: VocabularyEntry[];
  status: VocabularyBookStatus | null;
  loading: boolean;
  error: string;
  isDesktop: boolean;
  persist: (entry: VocabularyEntryInput) => Promise<VocabularyEntry>;
  remove: (id: number) => Promise<void>;
  refresh: () => Promise<void>;
  importBook: (path: string) => Promise<VocabularyBookStatus>;
  exportBook: (path: string) => Promise<VocabularyBookStatus>;
  moveBook: (path: string) => Promise<VocabularyBookStatus>;
  openBook: (path: string) => Promise<VocabularyBookStatus>;
  t: Translator;
}) {
  const [search, setSearch] = useState("");
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [creatingNew, setCreatingNew] = useState(false);
  const [draft, setDraft] = useState<VocabularyEntryInput>(emptyEntry);
  const [baseline, setBaseline] = useState<VocabularyEntryInput>(emptyEntry);
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState("");
  const [actionError, setActionError] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [mode, setMode] = useState<"read" | "edit">("read");
  const [fileMenu, setFileMenu] = useState(false);
  const [entryMenu, setEntryMenu] = useState<{ entryId: number; x: number; y: number } | null>(null);
  const fileMenuRef = useRef<HTMLDivElement>(null);
  const entryMenuRef = useRef<HTMLDivElement>(null);
  const confirmDeleteRef = useRef<HTMLButtonElement>(null);
  const previousId = useRef<number | null>(null);

  const visibleEntries = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    if (!query) return entries;
    return entries.filter((entry) => `${entry.word}\n${entry.phonetic}\n${entry.definitionMarkdown}\n${entry.examplesMarkdown}`.toLocaleLowerCase().includes(query));
  }, [entries, search]);
  const dirty = !sameEntry(draft, baseline);

  useEffect(() => {
    if (selectedId === null) return;
    const selected = entries.find((entry) => entry.id === selectedId);
    if (!selected || dirty) return;
    const next = editableEntry(selected);
    setDraft(next);
    setBaseline(next);
  }, [entries, selectedId, dirty]);

  useEffect(() => {
    if (loading || creatingNew || selectedId !== null || dirty || !entries.length) return;
    const next = editableEntry(entries[0]);
    setSelectedId(entries[0].id);
    setDraft(next);
    setBaseline(next);
  }, [creatingNew, dirty, entries, loading, selectedId]);

  const mayDiscard = () => !dirty || window.confirm(t("vocabularyDiscardChanges"));
  const selectEntry = (entry: VocabularyEntry) => {
    if (pending || !mayDiscard()) return false;
    const next = editableEntry(entry);
    setSelectedId(entry.id);
    setCreatingNew(false);
    setDraft(next);
    setBaseline(next);
    setMode("read");
    setMessage("");
    setActionError("");
    setConfirmDelete(false);
    return true;
  };
  const editEntry = (entry: VocabularyEntry) => {
    setEntryMenu(null);
    if (selectedId === entry.id && mode === "edit") return;
    if (selectEntry(entry)) setMode("edit");
  };
  const requestDelete = (entry: VocabularyEntry) => {
    setEntryMenu(null);
    if (!selectEntry(entry)) return;
    setConfirmDelete(true);
    requestAnimationFrame(() => confirmDeleteRef.current?.focus());
  };
  const openEntryMenu = (event: ReactMouseEvent<HTMLButtonElement>, entry: VocabularyEntry) => {
    event.preventDefault();
    event.stopPropagation();
    if (pending) return;
    const shell = event.currentTarget.closest<HTMLElement>(".app-shell");
    const bounds = shell?.getBoundingClientRect();
    const scale = shell && bounds ? bounds.width / shell.offsetWidth || 1 : 1;
    const x = bounds ? (event.clientX - bounds.left) / scale : event.clientX;
    const y = bounds ? (event.clientY - bounds.top) / scale : event.clientY;
    const width = shell?.clientWidth ?? window.innerWidth;
    const height = shell?.clientHeight ?? window.innerHeight;
    setFileMenu(false);
    setEntryMenu({ entryId: entry.id, x: Math.max(8, Math.min(x, width - 150)), y: Math.max(8, Math.min(y, height - 90)) });
  };
  const onEntryKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>, entry: VocabularyEntry) => {
    if ((event.key !== "Delete" && event.key !== "Backspace") || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    event.preventDefault();
    event.stopPropagation();
    requestDelete(entry);
  };
  const createEntry = () => {
    if (!mayDiscard()) return;
    previousId.current = selectedId;
    const next = emptyEntry();
    setSelectedId(null);
    setCreatingNew(true);
    setDraft(next);
    setBaseline(next);
    setMode("edit");
    setMessage("");
    setActionError("");
    setConfirmDelete(false);
  };
  const finishReading = () => {
    if (!mayDiscard()) return;
    setConfirmDelete(false);
    setMessage("");
    setActionError("");
    if (creatingNew) {
      setCreatingNew(false);
      const restore = entries.find((entry) => entry.id === previousId.current) ?? null;
      if (restore) {
        const next = editableEntry(restore);
        setSelectedId(restore.id);
        setDraft(next);
        setBaseline(next);
      } else {
        const next = emptyEntry();
        setSelectedId(null);
        setDraft(next);
        setBaseline(next);
      }
    } else setDraft(baseline);
    setMode("read");
  };
  const saveEntry = useCallback(async () => {
    setPending(true);
    setActionError("");
    setMessage("");
    try {
      const saved = await persist(draft);
      const next = editableEntry(saved);
      setSelectedId(saved.id);
      setCreatingNew(false);
      setDraft(next);
      setBaseline(next);
      setMode("read");
    } catch (reason) {
      setActionError(reason instanceof Error ? reason.message : String(reason));
    } finally { setPending(false); }
  }, [draft, persist]);
  const deleteEntry = async () => {
    if (draft.id === null) return;
    setPending(true);
    setActionError("");
    try {
      await remove(draft.id);
      const next = emptyEntry();
      setSelectedId(null);
      setCreatingNew(false);
      setDraft(next);
      setBaseline(next);
      setConfirmDelete(false);
      setMode("read");
      setMessage(t("vocabularyDeleted"));
    } catch (reason) {
      setActionError(reason instanceof Error ? reason.message : String(reason));
    } finally { setPending(false); }
  };
  const canSave = !pending && Boolean(draft.word.trim()) && dirty;
  useEffect(() => {
    if (mode !== "edit") return;
    const saveWithKeyboard = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLocaleLowerCase() !== "s") return;
      event.preventDefault();
      if (canSave) void saveEntry();
    };
    document.addEventListener("keydown", saveWithKeyboard);
    return () => document.removeEventListener("keydown", saveWithKeyboard);
  }, [canSave, mode, saveEntry]);
  useEffect(() => {
    if (!fileMenu) return;
    const close = (event: PointerEvent) => {
      if (!fileMenuRef.current?.contains(event.target as Node)) setFileMenu(false);
    };
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") setFileMenu(false); };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", onKey);
    };
  }, [fileMenu]);
  useEffect(() => {
    if (!entryMenu) return;
    entryMenuRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
    const closeOutside = (event: PointerEvent) => {
      if (!entryMenuRef.current?.contains(event.target as Node)) setEntryMenu(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setEntryMenu(null);
      document.querySelector<HTMLElement>(`.vocabulary-list > button[data-entry-id="${entryMenu.entryId}"]`)?.focus();
    };
    const closeOnScroll = () => setEntryMenu(null);
    document.addEventListener("pointerdown", closeOutside);
    document.addEventListener("keydown", closeOnEscape);
    document.addEventListener("scroll", closeOnScroll, true);
    window.addEventListener("resize", closeOnScroll);
    return () => {
      document.removeEventListener("pointerdown", closeOutside);
      document.removeEventListener("keydown", closeOnEscape);
      document.removeEventListener("scroll", closeOnScroll, true);
      window.removeEventListener("resize", closeOnScroll);
    };
  }, [entryMenu]);
  const chooseOpenPath = async (action: "open" | "import") => {
    if (!isDesktop || !mayDiscard()) return;
    const selected = await open({ multiple: false, directory: false, filters: sqliteFilters });
    if (typeof selected !== "string") return;
    setPending(true); setActionError(""); setMessage("");
    try {
      const nextStatus = action === "open" ? await openBook(selected) : await importBook(selected);
      await refresh();
      const next = emptyEntry();
      setSelectedId(null);
      setCreatingNew(false);
      setDraft(next);
      setBaseline(next);
      setConfirmDelete(false);
      setMode("read");
      setMessage(action === "open" ? t("vocabularyOpened") : `${t("vocabularyImported")} · ${nextStatus.entryCount}`);
    } catch (reason) { setActionError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setPending(false); }
  };
  const chooseSavePath = async (action: "move" | "export") => {
    if (!isDesktop || !mayDiscard()) return;
    const selected = await save({ defaultPath: action === "move" ? status?.path : "aurora-vocabulary.sqlite3", filters: sqliteFilters });
    if (!selected) return;
    setPending(true); setActionError(""); setMessage("");
    try {
      await (action === "move" ? moveBook(selected) : exportBook(selected));
      await refresh();
      setMessage(action === "move" ? t("vocabularyMoved") : t("vocabularyExported"));
    } catch (reason) { setActionError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setPending(false); }
  };
  const changeDraft = (key: keyof VocabularyEntryInput) => (event: ChangeEvent<HTMLInputElement>) => setDraft((current) => ({ ...current, [key]: event.target.value }));
  const deleteControls = draft.id !== null && (confirmDelete
    ? <><button ref={confirmDeleteRef} className="danger-button is-confirming" type="button" disabled={pending} onClick={() => void deleteEntry()}>{t("vocabularyConfirmDelete")}</button><button className="quiet-button" type="button" onClick={() => setConfirmDelete(false)}>{t("cancel")}</button></>
    : <button className="danger-button" type="button" disabled={pending} onClick={() => setConfirmDelete(true)}>{t("vocabularyDelete")}</button>);
  const showDetail = mode === "edit" || selectedId !== null;
  const runFileAction = (action: () => void) => { setFileMenu(false); action(); };

  return <section className="vocabulary-page" aria-label={t("vocabularyBook")}>
    <div className="vocabulary-toolbar">
      <div className="vocabulary-list-actions">
        <label><i className="fa-solid fa-magnifying-glass" aria-hidden="true" /><input value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t("vocabularySearch")} /></label>
        <button type="button" onClick={createEntry} title={t("vocabularyNew")} aria-label={t("vocabularyNew")}><i className="fa-solid fa-plus" aria-hidden="true" /></button>
      </div>
      {status && <span className="vocabulary-count">{status.entryCount} {t("vocabularyWordUnit")}</span>}
      <div className="vocabulary-file-menu" ref={fileMenuRef}>
        <button type="button" className="vocabulary-file-trigger" aria-expanded={fileMenu} aria-haspopup="menu" aria-label={t("vocabularyFiles")} onClick={() => setFileMenu((open) => !open)}><i className="fa-solid fa-ellipsis" aria-hidden="true" /></button>
        {fileMenu && <div className="vocabulary-file-popover" role="menu">
          <p>{status?.path || t("vocabularyPathUnavailable")}</p>
          <button type="button" role="menuitem" disabled={pending || !isDesktop} onClick={() => runFileAction(() => void chooseSavePath("move"))}><i className="fa-solid fa-folder-tree" aria-hidden="true" />{t("vocabularyMove")}</button>
          <button type="button" role="menuitem" disabled={pending || !isDesktop} onClick={() => runFileAction(() => void chooseOpenPath("open"))}><i className="fa-solid fa-folder-open" aria-hidden="true" />{t("vocabularyOpen")}</button>
          <button type="button" role="menuitem" disabled={pending || !isDesktop} onClick={() => runFileAction(() => void chooseOpenPath("import"))}><i className="fa-solid fa-file-import" aria-hidden="true" />{t("vocabularyImport")}</button>
          <button type="button" role="menuitem" disabled={pending || !isDesktop} onClick={() => runFileAction(() => void chooseSavePath("export"))}><i className="fa-solid fa-file-export" aria-hidden="true" />{t("vocabularyExport")}</button>
        </div>}
      </div>
    </div>
    {(error || actionError || message) && <div className={`vocabulary-feedback ${error || actionError ? "is-error" : "is-success"}`} role="status"><i className={`fa-solid ${error || actionError ? "fa-circle-exclamation" : "fa-circle-check"}`} aria-hidden="true" /><span>{error || actionError || message}</span></div>}
    <div className={`vocabulary-workspace${showDetail ? "" : " is-list-only"}${entries.length ? "" : " is-empty"}`}>
      <aside className="vocabulary-list-panel">
        <header className="vocabulary-card-heading"><h2>{t("vocabularySummary")}</h2></header>
        <div className="vocabulary-list">{loading ? <p className="vocabulary-list-empty">{t("loading")}</p> : visibleEntries.length ? visibleEntries.map((entry) => {
          const excerpt = definitionExcerpt(entry.definitionMarkdown);
          return <button type="button" data-entry-id={entry.id} className={selectedId === entry.id ? "is-active" : ""} aria-current={selectedId === entry.id ? "true" : undefined} onClick={() => { setEntryMenu(null); selectEntry(entry); }} onDoubleClick={() => editEntry(entry)} onContextMenu={(event) => openEntryMenu(event, entry)} onKeyDown={(event) => onEntryKeyDown(event, entry)} key={entry.id}><b>{entry.word}</b>{entry.phonetic && <small>{entry.phonetic}</small>}{excerpt && <span className="vocabulary-list-gloss">{excerpt}</span>}</button>;
        }) : <p className="vocabulary-list-empty">{search.trim() ? t("vocabularyNoMatches") : t("vocabularyEmpty")}</p>}</div>
      </aside>
      {showDetail && (mode === "edit" ? <section className="vocabulary-editor-card">
        <header className="vocabulary-card-heading"><h2>{t("vocabularyDetails")}</h2></header>
        <div className="vocabulary-word-fields"><label><span>{t("vocabularyWord")}</span><input value={draft.word} maxLength={120} autoCapitalize="none" onChange={changeDraft("word")} placeholder={t("vocabularyWordPlaceholder")} /></label><div className="vocabulary-phonetic-field"><label htmlFor="vocabulary-phonetic">{t("vocabularyPhonetic")}</label><div className="vocabulary-phonetic-control"><input id="vocabulary-phonetic" value={draft.phonetic} maxLength={200} onChange={changeDraft("phonetic")} placeholder="/ˌserənˈdɪpəti/" /><button type="button" disabled={!draft.word.trim()} onClick={() => void playPronunciation(draft.word, "en-US")} aria-label={t("vocabularyPronounce")} title={t("vocabularyPronounce")}><Icon name="speaker" size={15} /></button></div></div></div>
        <MarkdownField label={t("vocabularyDefinitionMarkdown")} value={draft.definitionMarkdown} onChange={(definitionMarkdown) => setDraft((current) => ({ ...current, definitionMarkdown }))} placeholder={t("vocabularyDefinitionPlaceholder")} emptyText={t("vocabularyPreviewEmpty")} template={`## ${t("definition")}\n\n- `} templateLabel={t("vocabularyInsertDefinition")} editLabel={t("vocabularyEdit")} previewLabel={t("vocabularyPreview")} />
        <MarkdownField label={t("vocabularyExamplesMarkdown")} value={draft.examplesMarkdown} onChange={(examplesMarkdown) => setDraft((current) => ({ ...current, examplesMarkdown }))} placeholder={t("vocabularyExamplesPlaceholder")} emptyText={t("vocabularyPreviewEmpty")} template={`## ${t("examples")}\n\n1. **English sentence.**\n   中文翻译。`} templateLabel={t("vocabularyInsertExample")} editLabel={t("vocabularyEdit")} previewLabel={t("vocabularyPreview")} />
        <div className="vocabulary-editor-actions"><div className="vocabulary-delete-actions">{deleteControls}</div><span className={dirty ? "is-dirty" : "is-saved"} role="status" aria-live="polite"><i aria-hidden="true" />{dirty ? t("vocabularyUnsaved") : t("vocabularyUpToDate")}</span>{!confirmDelete && <button className="quiet-button" type="button" onClick={finishReading}>{t("cancel")}</button>}<button className="primary-button vocabulary-save-button" type="button" disabled={!canSave} onClick={() => void saveEntry()}>{pending ? t("loading") : t("vocabularySave")}</button></div>
      </section> : <article className="vocabulary-reading">
        <header className="vocabulary-card-heading"><h2>{t("vocabularyDetails")}</h2></header>
        {draft.word ? <>
          <div className="word-line"><h3>{draft.word}</h3><PronunciationRow word={draft.word} fallbackPhonetic={draft.phonetic} t={t} /></div>
          {draft.definitionMarkdown.trim() && <div className="vocabulary-read-body"><ReadingMarkdown value={draft.definitionMarkdown} /></div>}
          {draft.examplesMarkdown.trim() && <div className="vocabulary-read-body"><ReadingMarkdown value={draft.examplesMarkdown} /></div>}
          <div className="vocabulary-read-actions"><button className="quiet-button" type="button" onClick={() => { setConfirmDelete(false); setMode("edit"); }}>{t("vocabularyEdit")}</button>{deleteControls}</div>
        </> : <p className="vocabulary-read-empty">{t("vocabularyChoose")}</p>}
      </article>)}
    </div>
    {entryMenu && entries.some((entry) => entry.id === entryMenu.entryId) && <div ref={entryMenuRef} className="vocabulary-entry-menu" role="menu" aria-label={entries.find((entry) => entry.id === entryMenu.entryId)?.word} style={{ left: entryMenu.x, top: entryMenu.y }} onKeyDown={(event) => {
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      event.preventDefault();
      const items = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("button")];
      const current = items.indexOf(document.activeElement as HTMLButtonElement);
      items[(current + (event.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
    }}>
      <button type="button" role="menuitem" onClick={() => { const entry = entries.find((item) => item.id === entryMenu.entryId); if (entry) editEntry(entry); }}><i className="fa-solid fa-pen" aria-hidden="true" />{t("vocabularyEdit")}</button>
      <button type="button" role="menuitem" className="is-danger" onClick={() => { const entry = entries.find((item) => item.id === entryMenu.entryId); if (entry) requestDelete(entry); }}><i className="fa-solid fa-trash" aria-hidden="true" />{t("vocabularyDelete")}</button>
    </div>}
  </section>;
}
