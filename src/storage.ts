import {
  DEFAULT_CACHE_LIMIT,
  DEFAULT_DICTIONARY_SYSTEM_PROMPT,
  DEFAULT_TRANSLATION_SYSTEM_PROMPT,
  LEGACY_DICTIONARY_SYSTEM_PROMPTS,
  LEGACY_TRANSLATION_SYSTEM_PROMPTS,
  MAX_ENABLED_SOURCES,
  QUERY_CACHE_DATABASE,
  QUERY_CACHE_STORE,
  SETTINGS_STORAGE_KEY,
  defaultSettings,
  localModels,
  sources,
  themes,
} from "./config";
import type { DisplaySettings, DownloadSourceId, QueryCacheRecord, SourceId, SourceLookupResult, UiLanguage } from "./types";

export function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(Math.max(value, minimum), maximum);
}

export function normaliseCacheLimit(value: number): number {
  return [50, 100, 200, 500].includes(value) ? value : DEFAULT_CACHE_LIMIT;
}

export function loadSettings(): DisplaySettings {
  try {
    const stored = JSON.parse(window.localStorage.getItem(SETTINGS_STORAGE_KEY) ?? "{}") as Partial<DisplaySettings>;
    const theme = themes.some((item) => item.id === stored.theme) ? stored.theme! : defaultSettings.theme;
    const language: UiLanguage = stored.language === "en" ? "en" : "zh";
    const scale = clamp(Number(stored.scale) || defaultSettings.scale, 0.5, 2);
    const cacheLimit = normaliseCacheLimit(Number(stored.cacheLimit));
    const font = typeof stored.font === "string" && stored.font.trim() ? stored.font : defaultSettings.font;
    const merriamWebsterApiKey = typeof stored.merriamWebsterApiKey === "string" ? stored.merriamWebsterApiKey.trim().slice(0, 128) : "";
    const localModel = localModels.some((model) => model.id === stored.localModel) ? stored.localModel! : defaultSettings.localModel;
    const llmDownloadSource: DownloadSourceId = stored.llmDownloadSource === "official" ? "official" : "mirror";
    const storedDictionaryPrompt = typeof stored.dictionarySystemPrompt === "string" ? stored.dictionarySystemPrompt.trim() : "";
    const dictionarySystemPrompt = storedDictionaryPrompt && !LEGACY_DICTIONARY_SYSTEM_PROMPTS.includes(storedDictionaryPrompt)
      ? storedDictionaryPrompt
      : DEFAULT_DICTIONARY_SYSTEM_PROMPT;
    const storedTranslationPrompt = typeof stored.translationSystemPrompt === "string" ? stored.translationSystemPrompt.trim() : "";
    const translationSystemPrompt = storedTranslationPrompt && !LEGACY_TRANSLATION_SYSTEM_PROMPTS.includes(storedTranslationPrompt)
      ? storedTranslationPrompt
      : DEFAULT_TRANSLATION_SYSTEM_PROMPT;
    const legacyVocabularyEnabled = Array.isArray(stored.enabledSources) && stored.enabledSources.includes("vocabulary" as SourceId);
    const storedSources = Array.isArray(stored.enabledSources)
      ? stored.enabledSources.filter((source, index): source is SourceId => sources.some((item) => item.id === source) && stored.enabledSources!.indexOf(source) === index).slice(0, MAX_ENABLED_SOURCES)
      : [...defaultSettings.enabledSources];
    const enabledSources = [...storedSources];
    if (legacyVocabularyEnabled) {
      for (const source of defaultSettings.enabledSources) {
        if (enabledSources.length >= MAX_ENABLED_SOURCES) break;
        if (!enabledSources.includes(source)) enabledSources.push(source);
      }
    }
    const storedOrder = Array.isArray(stored.sourceOrder)
      ? stored.sourceOrder.filter((source, index): source is SourceId => sources.some((item) => item.id === source) && stored.sourceOrder!.indexOf(source) === index)
      : [];
    return {
      theme, language, scale, cacheLimit, font, merriamWebsterApiKey, localModel, llmDownloadSource, dictionarySystemPrompt, translationSystemPrompt,
      enabledSources: enabledSources.length ? enabledSources : ["local"],
      sourceOrder: [...storedOrder, ...sources.map((source) => source.id).filter((source) => !storedOrder.includes(source))],
    };
  } catch {
    return cloneDefaultSettings();
  }
}

export function cloneDefaultSettings(): DisplaySettings {
  return { ...defaultSettings, enabledSources: [...defaultSettings.enabledSources], sourceOrder: [...defaultSettings.sourceOrder] };
}

export function persistSettings(settings: DisplaySettings): void {
  const enabledSources = [...new Set(settings.enabledSources)].slice(0, MAX_ENABLED_SOURCES);
  window.localStorage.setItem(SETTINGS_STORAGE_KEY, JSON.stringify({ ...settings, enabledSources: enabledSources.length ? enabledSources : ["local"] }));
}

function requestResult<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB request failed"));
  });
}

function transactionDone(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error ?? new Error("IndexedDB transaction failed"));
    transaction.onabort = () => reject(transaction.error ?? new Error("IndexedDB transaction aborted"));
  });
}

function openQueryCache(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (!("indexedDB" in window)) return reject(new Error("IndexedDB is unavailable"));
    const request = window.indexedDB.open(`${QUERY_CACHE_DATABASE}-case-sensitive-v2`, 1);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(QUERY_CACHE_STORE)) database.createObjectStore(QUERY_CACHE_STORE, { keyPath: "query" });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("Unable to open query cache"));
  });
}

function cacheKey(query: string): string {
  return query.trim();
}

export async function readCachedQuery(query: string): Promise<QueryCacheRecord | null> {
  const key = cacheKey(query);
  if (!key) return null;
  let database: IDBDatabase | undefined;
  try {
    database = await openQueryCache();
    const transaction = database.transaction(QUERY_CACHE_STORE, "readonly");
    const done = transactionDone(transaction);
    void done.catch(() => undefined);
    const record = await requestResult<QueryCacheRecord | undefined>(transaction.objectStore(QUERY_CACHE_STORE).get(key));
    await done;
    database.close();
    if (record) void touchCachedQuery(record.query);
    return record ?? null;
  } catch {
    return null;
  } finally { database?.close(); }
}

async function touchCachedQuery(query: string): Promise<void> {
  let database: IDBDatabase | undefined;
  try {
    database = await openQueryCache();
    const transaction = database.transaction(QUERY_CACHE_STORE, "readwrite");
    const done = transactionDone(transaction);
    void done.catch(() => undefined);
    const store = transaction.objectStore(QUERY_CACHE_STORE);
    const record = await requestResult<QueryCacheRecord | undefined>(store.get(query));
    if (record) store.put({ ...record, accessedAt: Date.now() });
    await done;
    database.close();
  } catch { /* Cache access is optional. */ } finally { database?.close(); }
}

export async function cacheSourceResults(query: string, results: Partial<Record<SourceId, SourceLookupResult>>, limit: number): Promise<void> {
  const key = cacheKey(query);
  if (!key || !Object.keys(results).length) return;
  let database: IDBDatabase | undefined;
  try {
    database = await openQueryCache();
    const transaction = database.transaction(QUERY_CACHE_STORE, "readwrite");
    const done = transactionDone(transaction);
    void done.catch(() => undefined);
    const store = transaction.objectStore(QUERY_CACHE_STORE);
    const previous = await requestResult<QueryCacheRecord | undefined>(store.get(key));
    store.put({ query: key, accessedAt: Date.now(), results: { ...(previous?.results ?? {}), ...results } } satisfies QueryCacheRecord);
    await done;
    database.close();
    await trimQueryCache(limit);
  } catch { /* Cache failures never block lookup. */ } finally { database?.close(); }
}

export async function trimQueryCache(limit: number): Promise<void> {
  let database: IDBDatabase | undefined;
  try {
    database = await openQueryCache();
    const transaction = database.transaction(QUERY_CACHE_STORE, "readwrite");
    const done = transactionDone(transaction);
    void done.catch(() => undefined);
    const store = transaction.objectStore(QUERY_CACHE_STORE);
    const records = await requestResult<QueryCacheRecord[]>(store.getAll());
    records.sort((left, right) => right.accessedAt - left.accessedAt).slice(limit).forEach((record) => store.delete(record.query));
    await done;
    database.close();
  } catch { /* Best effort. */ } finally { database?.close(); }
}
