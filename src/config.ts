import type { DisplaySettings, FontId, LocalLookup, LocalModelId, SourceId, ThemeId } from "./types";

import translations from "../config/locales.json";
import promptConfiguration from "../config/prompts.json";

export type CopyKey = keyof typeof translations.zh;
const englishCopy: Record<CopyKey, string> = translations.en;
export const copy = { zh: translations.zh, en: englishCopy } as const;
export const sources: Array<{ id: SourceId; title: CopyKey; subtitle: CopyKey }> = [
  { id: "local", title: "localSourceTitle", subtitle: "localSourceSubtitle" },
  { id: "local_llm", title: "localLlmSourceTitle", subtitle: "localLlmSourceSubtitle" },
  { id: "youdao", title: "youdaoSourceTitle", subtitle: "youdaoSourceSubtitle" },
  { id: "dictionary", title: "dictionarySourceTitle", subtitle: "dictionarySourceSubtitle" },
  { id: "merriam_webster", title: "merriamWebsterSourceTitle", subtitle: "merriamWebsterSourceSubtitle" },
];

export const localModels: Array<{ id: LocalModelId; name: string; description: CopyKey; footprint: CopyKey; recommended?: boolean }> = [
  { id: "qwen3-0.6b", name: "Qwen3-0.6B", description: "qwen3_0_6bDescription", footprint: "modelLightweight", recommended: true },
  { id: "qwen3-1.7b", name: "Qwen3-1.7B", description: "qwen3_1_7bDescription", footprint: "modelBalanced" },
  { id: "qwen3-4b", name: "Qwen3-4B", description: "qwen3_4bDescription", footprint: "modelHighQuality" },
];

export const themes: Array<{ id: ThemeId; label: CopyKey; color: string }> = [
  { id: "red", label: "themeRed", color: "#df5d68" }, { id: "orange", label: "themeOrange", color: "#eb8c43" },
  { id: "yellow", label: "themeYellow", color: "#d9a91c" }, { id: "green", label: "themeGreen", color: "#49a86e" },
  { id: "cyan", label: "themeCyan", color: "#087977" }, { id: "blue", label: "themeBlue", color: "#4e86d8" },
  { id: "purple", label: "themePurple", color: "#706bdd" }, { id: "violet", label: "themeViolet", color: "#9a67cd" },
  { id: "black", label: "themeBlack", color: "#112831" },
];

export const SYSTEM_FONT_ID = "system";
export const systemFontStacks = {
  windows: 'system-ui, "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", sans-serif',
  macos: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif',
  linux: 'system-ui, "Noto Sans", "Noto Sans CJK SC", sans-serif',
} as const;
export const SETTINGS_STORAGE_KEY = "aurora-dictionary-display-settings";
export const QUERY_CACHE_DATABASE = "aurora-dictionary-query-cache";
export const QUERY_CACHE_STORE = "query-results";
export const DEFAULT_CACHE_LIMIT = 100;
export const CACHE_LIMIT_OPTIONS = [50, 100, 200, 500] as const;
export const MAX_ENABLED_SOURCES = 4;
export const DEFAULT_DICTIONARY_SYSTEM_PROMPT = promptConfiguration.dictionarySystemPrompt;
export const DEFAULT_TRANSLATION_SYSTEM_PROMPT = promptConfiguration.translationSystemPrompt;
export const LEGACY_DICTIONARY_SYSTEM_PROMPTS: readonly string[] = promptConfiguration.legacyDictionarySystemPrompts;
export const LEGACY_TRANSLATION_SYSTEM_PROMPTS: readonly string[] = promptConfiguration.legacyTranslationSystemPrompts;

export const defaultSettings: DisplaySettings = {
  theme: "cyan", language: "zh", scale: 1, font: SYSTEM_FONT_ID,
  enabledSources: ["local", "local_llm", "youdao", "dictionary"], sourceOrder: sources.map((source) => source.id),
  merriamWebsterApiKey: "",
  localModel: "qwen3-0.6b", llmDownloadSource: "mirror", dictionarySystemPrompt: DEFAULT_DICTIONARY_SYSTEM_PROMPT,
  translationSystemPrompt: DEFAULT_TRANSLATION_SYSTEM_PROMPT, cacheLimit: DEFAULT_CACHE_LIMIT,
};

export const fallbackLookup: LocalLookup = {
  query: "", sampleData: true, suggestions: [],
  entries: [{ word: "serendipity", phonetic: "/ˌserənˈdɪpəti/", pos: "n.", translation: "n. 意外发现珍奇事物的本领；机缘巧合", definition: "the faculty or phenomenon of finding valuable things not sought for" }],
};

export const initialFonts: Array<{ id: FontId; label: string }> = [{ id: SYSTEM_FONT_ID, label: "" }];
